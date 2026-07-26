//! Reading derived state back out of SQLite, and assembling `SessionView`s.
//!
//! Liveness, git status, and the state machine all run *here*, at read time,
//! rather than being stored. That is what removes the need for a daemon: there
//! is no timer that has to sweep sessions into a `STALE` bucket, because
//! staleness is just `now - last_event_at`.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row};

use crate::config::Config;
use crate::model::*;
use crate::state;

const SESSION_COLUMNS: &str = "session_id, project_path, project_slug, transcript_path, summary,
     first_prompt, git_branch, source, started_at, last_event_at, last_prompt_at, last_stop_at,
     last_notif_at, last_notif_kind, last_event_type, ended_at, end_reason, pid, term_program,
     term_session_id, tool_calls, ticket_keys, runtime_status, waiting_for, proc_start,
     session_name, runtime_kind";

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    let ts = |i: usize| -> rusqlite::Result<Option<DateTime<Utc>>> {
        Ok(row
            .get::<_, Option<String>>(i)?
            .as_deref()
            .and_then(parse_ts))
    };
    let ticket_keys: Option<String> = row.get(21)?;
    Ok(Session {
        session_id: row.get(0)?,
        project_path: row.get(1)?,
        project_slug: row.get(2)?,
        transcript_path: row.get(3)?,
        summary: row.get(4)?,
        first_prompt: row.get(5)?,
        git_branch: row.get(6)?,
        source: row.get(7)?,
        started_at: ts(8)?,
        last_event_at: ts(9)?,
        last_prompt_at: ts(10)?,
        last_stop_at: ts(11)?,
        last_notif_at: ts(12)?,
        last_notif_kind: row.get(13)?,
        last_event_type: row.get(14)?,
        ended_at: ts(15)?,
        end_reason: row.get(16)?,
        pid: row.get(17)?,
        term_program: row.get(18)?,
        term_session_id: row.get(19)?,
        tool_calls: row.get(20)?,
        ticket_keys: ticket_keys
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        runtime_status: row.get(22)?,
        waiting_for: row.get(23)?,
        proc_start: ts(24)?,
        session_name: row.get(25)?,
        runtime_kind: row.get(26)?,
    })
}

pub fn load_sessions(conn: &Connection) -> Result<Vec<Session>> {
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], session_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn load_session(conn: &Connection, session_id: &str) -> Result<Option<Session>> {
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map([session_id], session_from_row)?;
    Ok(rows.next().transpose()?)
}

pub fn files_for(conn: &Connection, session_id: &str) -> Result<Vec<FileEdit>> {
    let mut stmt = conn.prepare(
        "SELECT path, edits FROM session_files WHERE session_id = ?1
         ORDER BY edits DESC, path ASC",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(FileEdit {
            path: r.get(0)?,
            edits: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Repos each session actually edited files in, most-edited first.
pub fn load_session_projects(conn: &Connection) -> Result<HashMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, project_path FROM session_projects
         ORDER BY session_id, edits DESC, project_path ASC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (session_id, project) = row?;
        out.entry(session_id).or_default().push(project);
    }
    Ok(out)
}

/// Per-project edit counts for one session, most-edited first.
pub fn projects_for(conn: &Connection, session_id: &str) -> Result<Vec<ProjectEdits>> {
    let mut stmt = conn.prepare(
        "SELECT project_path, edits FROM session_projects WHERE session_id = ?1
         ORDER BY edits DESC, project_path ASC",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(ProjectEdits {
            project_path: r.get(0)?,
            edits: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn todos_for(conn: &Connection, session_id: &str) -> Result<Vec<Todo>> {
    let mut stmt =
        conn.prepare("SELECT content, status FROM session_todos WHERE session_id = ?1")?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(Todo {
            content: r.get(0)?,
            status: TodoStatus::parse(&r.get::<_, String>(1)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Open (pending or in-progress) task counts for every session, in one query.
pub fn open_todo_counts(conn: &Connection) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, count(*) FROM session_todos
         WHERE status != 'completed' GROUP BY session_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

pub fn commits_for(conn: &Connection, session_id: &str) -> Result<Vec<AttributedCommit>> {
    let mut stmt = conn.prepare(
        "SELECT c.sha, c.project_path, c.ts, c.subject, c.author_email, c.branch, sc.confidence
         FROM session_commits sc JOIN commits c ON c.sha = sc.sha
         WHERE sc.session_id = ?1
         ORDER BY c.ts ASC",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        let ts: String = r.get(2)?;
        Ok(AttributedCommit {
            commit: Commit {
                sha: r.get(0)?,
                project_path: r.get(1)?,
                ts: parse_ts(&ts).unwrap_or_else(Utc::now),
                subject: r.get(3)?,
                author_email: r.get(4)?,
                branch: r.get(5)?,
            },
            confidence: Confidence::parse(&r.get::<_, String>(6)?),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Live git status per project, computed once per project and shared.
pub fn project_git_status(
    cfg: &Config,
    project_paths: &[String],
) -> HashMap<String, crate::git::RepoInfo> {
    project_paths
        .iter()
        .map(|p| {
            let info = crate::git::repo_info(
                std::path::Path::new(p),
                cfg.git_timeout_secs,
                cfg.git_cache_ttl_secs,
            );
            (p.clone(), info)
        })
        .collect()
}

/// Assemble every session with its derived state, sorted most-urgent first.
pub fn build_views(
    conn: &Connection,
    cfg: &Config,
    now: DateTime<Utc>,
) -> Result<Vec<SessionView>> {
    let sessions = load_sessions(conn)?;
    let todo_counts = open_todo_counts(conn)?;
    let links = load_session_projects(conn)?;

    let mut project_paths: Vec<String> = links.values().flatten().cloned().collect();
    project_paths.extend(sessions.iter().map(|s| s.project_path.clone()));
    project_paths.sort();
    project_paths.dedup();
    let git_status = project_git_status(cfg, &project_paths);

    let mut views = Vec::with_capacity(sessions.len());
    for session in sessions {
        if cfg.is_excluded(&session.project_path) {
            continue;
        }
        let liveness =
            state::check_liveness(session.pid, session.proc_start, session.last_event_at, now);
        let projects = projects_for(conn, &session.session_id)?;
        // Dirtiness is judged against the repo the work actually happened in,
        // not the directory `claude` was launched from.
        let primary = projects
            .first()
            .map(|p| p.project_path.clone())
            .unwrap_or_else(|| session.project_path.clone());
        let dirty = git_status.get(&primary).and_then(|g| g.dirty);
        let open_todos = todo_counts.get(&session.session_id).copied().unwrap_or(0);

        let derived = state::derive(state::Inputs {
            session: &session,
            now,
            liveness,
            worktree_dirty: dirty,
            open_todos,
            cfg,
        });

        let files = files_for(conn, &session.session_id)?;
        let commits = commits_for(conn, &session.session_id)?;

        views.push(SessionView {
            state: derived.state,
            stale: derived.stale,
            action_kind: derived.action_kind,
            liveness,
            worktree_dirty: dirty,
            open_todos,
            files,
            commits,
            projects,
            session,
        });
    }

    views.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then(b.session.last_event_at.cmp(&a.session.last_event_at))
    });
    Ok(views)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn seed(conn: &Connection, id: &str, project: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, project_path, project_slug, summary,
                                   last_event_at, last_prompt_at, last_stop_at, tool_calls)
             VALUES (?1, ?2, '-p', 'A title', ?3, ?4, ?5, 3)",
            rusqlite::params![
                id,
                project,
                "2026-07-24T10:00:00.000Z",
                "2026-07-24T09:00:00.000Z",
                "2026-07-24T10:00:00.000Z",
            ],
        )
        .unwrap();
    }

    #[test]
    fn round_trips_a_session() {
        let conn = db::open_memory().unwrap();
        seed(&conn, "s1", "/p");
        let s = load_session(&conn, "s1").unwrap().unwrap();
        assert_eq!(s.session_id, "s1");
        assert_eq!(s.display_title(), "A title");
        assert_eq!(s.tool_calls, 3);
        assert!(s.last_event_at.is_some());
        assert!(load_session(&conn, "nope").unwrap().is_none());
    }

    #[test]
    fn display_title_falls_back_through_name_then_id() {
        let mut s = Session {
            session_id: "abc".into(),
            ..Default::default()
        };
        assert_eq!(s.display_title(), "abc");
        s.session_name = Some("derived-name".into());
        assert_eq!(s.display_title(), "derived-name");
        s.summary = Some("AI Title".into());
        assert_eq!(s.display_title(), "AI Title");
    }

    #[test]
    fn open_todo_counts_ignore_completed() {
        let conn = db::open_memory().unwrap();
        for (content, status) in [("a", "pending"), ("b", "in_progress"), ("c", "completed")] {
            conn.execute(
                "INSERT INTO session_todos (session_id, content, status) VALUES ('s1',?1,?2)",
                [content, status],
            )
            .unwrap();
        }
        let counts = open_todo_counts(&conn).unwrap();
        assert_eq!(counts.get("s1"), Some(&2));
    }

    #[test]
    fn excluded_projects_never_appear() {
        let conn = db::open_memory().unwrap();
        seed(&conn, "s1", "/Users/x/scratch/thing");
        seed(&conn, "s2", "/Users/x/real");
        let cfg = Config {
            exclude_projects: vec!["/scratch".into()],
            ..Default::default()
        };
        let views = build_views(&conn, &cfg, Utc::now()).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].session.session_id, "s2");
    }

    #[test]
    fn views_sort_most_urgent_first() {
        let conn = db::open_memory().unwrap();
        seed(&conn, "idle", "/p");
        seed(&conn, "waiting", "/p");
        conn.execute(
            "UPDATE sessions SET runtime_status='waiting', waiting_for='permission prompt', pid=?1, proc_start=?2 WHERE session_id='waiting'",
            rusqlite::params![std::process::id() as i64, format_ts(&Utc::now())],
        )
        .unwrap();

        let views = build_views(&conn, &Config::default(), Utc::now()).unwrap();
        assert_eq!(views[0].session.session_id, "waiting");
        assert_eq!(views[0].state, SessionState::NeedsAction);
    }
}
