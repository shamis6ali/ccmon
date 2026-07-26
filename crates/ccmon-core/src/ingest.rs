//! Pulling every source into SQLite.
//!
//! Order of operations:
//!   1. drain the spool into `events`
//!   2. parse transcripts whose mtime changed
//!   3. read Claude Code's runtime files (pid, live status)
//!   4. read per-session task lists
//!   5. roll `events` up onto `sessions`
//!   6. collect git per project, attribute commits, extract ticket keys
//!
//! Safe to run concurrently from two processes: WAL plus `busy_timeout` covers
//! the overlap, and every write is an idempotent upsert.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use rusqlite::{params, Connection};

use crate::config::Config;
use crate::model::*;
use crate::{db, git, runtime, spool, todos, transcript};

#[derive(Debug, Default, Clone)]
pub struct IngestStats {
    pub spool: spool::DrainStats,
    pub transcripts_seen: usize,
    pub transcripts_parsed: usize,
    pub transcript_lines_skipped: usize,
    pub runtime_files: usize,
    pub sessions: usize,
    pub projects: usize,
    pub commits: usize,
    pub attributions: usize,
}

/// Incremental ingest. Only transcripts whose mtime changed are re-parsed.
pub fn run(conn: &Connection, cfg: &Config) -> Result<IngestStats> {
    run_inner(conn, cfg, false)
}

/// Full rebuild: drop every derived row and re-derive from `events` plus
/// transcripts. `events` and spool offsets are untouched.
pub fn reindex(conn: &Connection, cfg: &Config) -> Result<IngestStats> {
    db::clear_derived(conn)?;
    db::clear_ingest_state_prefix(conn, "transcript_mtime:")?;
    git::clear_cache();
    run_inner(conn, cfg, true)
}

fn run_inner(conn: &Connection, cfg: &Config, force: bool) -> Result<IngestStats> {
    let mut stats = IngestStats::default();

    // 1. Spool.
    if let Ok(spool_path) = crate::paths::spool_path() {
        let _ = spool::rotate_if_needed(&spool_path, cfg.spool_max_bytes);
        stats.spool = retry_busy(|| spool::drain(conn, &spool_path))?;
    }

    let discovery = cfg.all_roots();

    // 2. Transcripts.
    for root in &discovery.found {
        scan_transcripts(conn, cfg, &root.projects_dir(), force, &mut stats)?;
    }

    // 3. Runtime files: pid, process start, and live status.
    for root in &discovery.found {
        for rs in runtime::scan(&root.path.join("sessions")) {
            stats.runtime_files += 1;
            upsert_runtime(conn, &rs)?;
        }
    }

    // 4. Task lists.
    for root in &discovery.found {
        for (session_id, list) in todos::scan_root(&root.path) {
            replace_todos(conn, &session_id, &list)?;
        }
    }

    // 5. Roll events up onto sessions.
    rollup_events(conn)?;

    // 6. Work out which repos each session actually worked in.
    resolve_session_projects(conn, cfg)?;

    // 7. Git.
    collect_git(conn, cfg, &mut stats)?;

    let count: i64 = conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;
    stats.sessions = count as usize;
    Ok(stats)
}

/// Retry once on SQLITE_BUSY; WAL plus busy_timeout handles the rest.
fn retry_busy<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    match f() {
        Ok(v) => Ok(v),
        Err(e) => {
            let busy = e
                .downcast_ref::<rusqlite::Error>()
                .map(|e| {
                    matches!(
                        e.sqlite_error_code(),
                        Some(rusqlite::ErrorCode::DatabaseBusy)
                            | Some(rusqlite::ErrorCode::DatabaseLocked)
                    )
                })
                .unwrap_or(false);
            if busy {
                std::thread::sleep(std::time::Duration::from_millis(150));
                f()
            } else {
                Err(e)
            }
        }
    }
}

// --- transcripts -----------------------------------------------------------

fn scan_transcripts(
    conn: &Connection,
    cfg: &Config,
    projects_dir: &Path,
    force: bool,
    stats: &mut IngestStats,
) -> Result<()> {
    let dirs = match std::fs::read_dir(projects_dir) {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };

    for project in dirs.flatten() {
        let project_dir = project.path();
        if !project_dir.is_dir() {
            continue;
        }
        let slug = project_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let entries = match std::fs::read_dir(&project_dir) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let mut work: Vec<(std::path::PathBuf, Option<String>)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `<session-uuid>/subagents/**/agent-*.jsonl` — work this
                // session did through subagents. It nests arbitrarily deep
                // (a workflow spawns agents that spawn agents), so collect
                // recursively and attribute it all to the parent session.
                if let Some(parent) = path.file_name().and_then(|s| s.to_str()) {
                    let parent = parent.to_string();
                    let mut found = Vec::new();
                    collect_jsonl(&path, &mut found);
                    work.extend(found.into_iter().map(|p| (p, Some(parent.clone()))));
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                work.push((path, None));
            }
        }

        for (path, subagent_of) in work {
            stats.transcripts_seen += 1;

            let mtime = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default();
            let key = format!("transcript_mtime:{}", path.display());
            if !force && db::get_state(conn, &key).as_deref() == Some(mtime.as_str()) {
                continue;
            }

            let parsed = match transcript::parse_file(&path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(path = %path.display(), error = %e, "unreadable transcript");
                    continue;
                }
            };
            stats.transcripts_parsed += 1;
            stats.transcript_lines_skipped += parsed.lines_skipped;

            if let Err(e) = apply_transcript(conn, cfg, &parsed, &slug, subagent_of.as_deref()) {
                tracing::warn!(path = %path.display(), error = %e, "failed to apply transcript");
                continue;
            }
            db::set_state(conn, &key, &mtime)?;
        }
    }
    Ok(())
}

/// Every `.jsonl` beneath `dir`, at any depth.
fn collect_jsonl(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Apply one parsed transcript.
///
/// `subagent_of` names the parent session when this is a subagent's transcript.
/// Those carry real work — files edited, tools run — and it belongs to the
/// parent session, otherwise a session that delegated everything to agents
/// reports zero files. What they must *not* contribute is identity: a
/// subagent's first message is the instruction we gave it, not something the
/// user typed, and its title is not the session's title.
fn apply_transcript(
    conn: &Connection,
    cfg: &Config,
    parsed: &transcript::ParsedTranscript,
    slug: &str,
    subagent_of: Option<&str>,
) -> Result<()> {
    let Some(session_id) = subagent_of
        .map(str::to_string)
        .or_else(|| parsed.session_id.clone())
    else {
        return Ok(());
    };
    let is_subagent = subagent_of.is_some();

    // The real cwd comes from the transcript itself. The directory slug is a
    // lossy mangling of it and is never reversed.
    let existing = crate::store::load_session(conn, &session_id)?;
    let project_path = parsed
        .cwd
        .clone()
        .or_else(|| existing.as_ref().map(|e| e.project_path.clone()));
    let Some(project_path) = project_path else {
        tracing::debug!(%session_id, "transcript has no cwd; skipping");
        return Ok(());
    };
    if cfg.is_excluded(&project_path) {
        return Ok(());
    }

    let incoming = if is_subagent {
        Session {
            session_id: session_id.clone(),
            project_path,
            project_slug: slug.to_string(),
            // Activity and edited files only. Turn boundaries are the parent's
            // to define, so a subagent's messages must not open or close the
            // parent's turn. `tool_calls` stays the session's own count,
            // because the merge takes a max and summing across sources would
            // double count.
            last_event_at: parsed.last_ts,
            ..Default::default()
        }
    } else {
        Session {
            session_id: session_id.clone(),
            project_path,
            project_slug: slug.to_string(),
            transcript_path: Some(parsed.path.display().to_string()),
            summary: parsed.summary.clone(),
            first_prompt: parsed.first_prompt.clone(),
            git_branch: parsed.git_branch.clone(),
            started_at: parsed.first_ts,
            last_event_at: parsed.last_ts,
            last_prompt_at: parsed.last_user_ts,
            last_stop_at: parsed.last_assistant_ts,
            tool_calls: parsed.tool_calls,
            ..Default::default()
        }
    };
    upsert_session(conn, existing.as_ref(), &incoming)?;

    let mut stmt = conn.prepare_cached(
        "INSERT INTO session_files (session_id, path, edits) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id, path) DO UPDATE SET edits = max(edits, excluded.edits)",
    )?;
    for (path, edits) in &parsed.files {
        stmt.execute(params![session_id, crate::paths::normalize(path), edits])?;
    }
    Ok(())
}

/// Merge an incoming partial session onto whatever is already stored.
///
/// Field-by-field rather than a SQL upsert, because the rules genuinely differ
/// per field: earliest wins for `started_at`, latest for activity, and the
/// *first* prompt must never be overwritten by a later one.
fn upsert_session(conn: &Connection, existing: Option<&Session>, incoming: &Session) -> Result<()> {
    let merged = match existing {
        None => incoming.clone(),
        Some(old) => Session {
            session_id: incoming.session_id.clone(),
            project_path: pick(&incoming.project_path, &old.project_path),
            project_slug: pick(&incoming.project_slug, &old.project_slug),
            transcript_path: or_keep(&incoming.transcript_path, &old.transcript_path),
            summary: or_keep(&incoming.summary, &old.summary),
            // First prompt is first-write-wins by definition.
            first_prompt: or_keep(&old.first_prompt, &incoming.first_prompt),
            git_branch: or_keep(&incoming.git_branch, &old.git_branch),
            source: or_keep(&incoming.source, &old.source),
            started_at: min_opt(old.started_at, incoming.started_at),
            last_event_at: max_opt(old.last_event_at, incoming.last_event_at),
            last_prompt_at: max_opt(old.last_prompt_at, incoming.last_prompt_at),
            last_stop_at: max_opt(old.last_stop_at, incoming.last_stop_at),
            last_notif_at: max_opt(old.last_notif_at, incoming.last_notif_at),
            last_notif_kind: or_keep(&incoming.last_notif_kind, &old.last_notif_kind),
            last_event_type: or_keep(&incoming.last_event_type, &old.last_event_type),
            ended_at: max_opt(old.ended_at, incoming.ended_at),
            end_reason: or_keep(&incoming.end_reason, &old.end_reason),
            pid: incoming.pid.or(old.pid),
            term_program: or_keep(&incoming.term_program, &old.term_program),
            term_session_id: or_keep(&incoming.term_session_id, &old.term_session_id),
            // Both sources count the same underlying tool calls, so take the
            // larger rather than summing and double counting.
            tool_calls: incoming.tool_calls.max(old.tool_calls),
            ticket_keys: if incoming.ticket_keys.is_empty() {
                old.ticket_keys.clone()
            } else {
                incoming.ticket_keys.clone()
            },
            runtime_status: or_keep(&incoming.runtime_status, &old.runtime_status),
            waiting_for: or_keep(&incoming.waiting_for, &old.waiting_for),
            proc_start: incoming.proc_start.or(old.proc_start),
            session_name: or_keep(&incoming.session_name, &old.session_name),
            runtime_kind: or_keep(&incoming.runtime_kind, &old.runtime_kind),
        },
    };
    write_session(conn, &merged)
}

fn pick(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        a.to_string()
    }
}

fn or_keep(preferred: &Option<String>, fallback: &Option<String>) -> Option<String> {
    preferred.clone().or_else(|| fallback.clone())
}

fn write_session(conn: &Connection, s: &Session) -> Result<()> {
    let ts = |t: &Option<DateTime<Utc>>| t.as_ref().map(format_ts);
    conn.execute(
        "INSERT INTO sessions (
            session_id, project_path, project_slug, transcript_path, summary, first_prompt,
            git_branch, source, started_at, last_event_at, last_prompt_at, last_stop_at,
            last_notif_at, last_notif_kind, last_event_type, ended_at, end_reason, pid,
            term_program, term_session_id, tool_calls, ticket_keys, runtime_status,
            waiting_for, proc_start, session_name, runtime_kind)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
         ON CONFLICT(session_id) DO UPDATE SET
            project_path=excluded.project_path, project_slug=excluded.project_slug,
            transcript_path=excluded.transcript_path, summary=excluded.summary,
            first_prompt=excluded.first_prompt, git_branch=excluded.git_branch,
            source=excluded.source, started_at=excluded.started_at,
            last_event_at=excluded.last_event_at, last_prompt_at=excluded.last_prompt_at,
            last_stop_at=excluded.last_stop_at, last_notif_at=excluded.last_notif_at,
            last_notif_kind=excluded.last_notif_kind, last_event_type=excluded.last_event_type,
            ended_at=excluded.ended_at, end_reason=excluded.end_reason, pid=excluded.pid,
            term_program=excluded.term_program, term_session_id=excluded.term_session_id,
            tool_calls=excluded.tool_calls, ticket_keys=excluded.ticket_keys,
            runtime_status=excluded.runtime_status, waiting_for=excluded.waiting_for,
            proc_start=excluded.proc_start, session_name=excluded.session_name,
            runtime_kind=excluded.runtime_kind",
        params![
            s.session_id,
            s.project_path,
            s.project_slug,
            s.transcript_path,
            s.summary,
            s.first_prompt,
            s.git_branch,
            s.source,
            ts(&s.started_at),
            ts(&s.last_event_at),
            ts(&s.last_prompt_at),
            ts(&s.last_stop_at),
            ts(&s.last_notif_at),
            s.last_notif_kind,
            s.last_event_type,
            ts(&s.ended_at),
            s.end_reason,
            s.pid,
            s.term_program,
            s.term_session_id,
            s.tool_calls,
            serde_json::to_string(&s.ticket_keys).ok(),
            s.runtime_status,
            s.waiting_for,
            ts(&s.proc_start),
            s.session_name,
            s.runtime_kind,
        ],
    )?;
    Ok(())
}

// --- runtime files ---------------------------------------------------------

fn upsert_runtime(conn: &Connection, rs: &runtime::RuntimeSession) -> Result<()> {
    let Some(session_id) = rs.session_id.clone() else {
        return Ok(());
    };
    let existing = crate::store::load_session(conn, &session_id)?;
    let project_path = rs
        .cwd
        .clone()
        .or_else(|| existing.as_ref().map(|e| e.project_path.clone()))
        .unwrap_or_default();
    if project_path.is_empty() {
        return Ok(());
    }

    let incoming = Session {
        session_id,
        project_path: project_path.clone(),
        project_slug: existing
            .as_ref()
            .map(|e| e.project_slug.clone())
            .unwrap_or_else(|| crate::paths::slug_for_path(&project_path)),
        started_at: rs.started_at_utc(),
        // The runtime file is rewritten on every status change, so its
        // updatedAt is a genuine liveness heartbeat.
        last_event_at: rs.updated_at_utc(),
        pid: rs.pid,
        proc_start: rs.proc_start.as_deref().and_then(runtime::parse_proc_start),
        runtime_status: rs.status.clone(),
        waiting_for: rs.waiting_for.clone(),
        session_name: rs.name.clone(),
        runtime_kind: rs.kind.clone(),
        ..Default::default()
    };
    upsert_session(conn, existing.as_ref(), &incoming)
}

fn replace_todos(conn: &Connection, session_id: &str, list: &[Todo]) -> Result<()> {
    conn.execute(
        "DELETE FROM session_todos WHERE session_id = ?1",
        [session_id],
    )?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO session_todos (session_id, content, status) VALUES (?1,?2,?3)",
    )?;
    for t in list {
        stmt.execute(params![session_id, t.content, t.status.as_str()])?;
    }
    Ok(())
}

// --- event rollup ----------------------------------------------------------

fn rollup_events(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT session_id,
                min(ts),
                max(ts),
                max(CASE WHEN event_type = 'UserPromptSubmit' THEN ts END),
                max(CASE WHEN event_type IN ('Stop','SubagentStop') THEN ts END),
                max(CASE WHEN event_type = 'Notification' THEN ts END),
                max(CASE WHEN event_type = 'SessionEnd' THEN ts END),
                sum(CASE WHEN event_type = 'PostToolUse' THEN 1 ELSE 0 END),
                max(cwd), max(transcript_path), max(ppid),
                max(term_program), max(term_session_id)
         FROM events GROUP BY session_id",
    )?;

    struct Agg {
        session_id: String,
        first: Option<String>,
        last: Option<String>,
        prompt: Option<String>,
        stop: Option<String>,
        notif: Option<String>,
        end: Option<String>,
        tools: i64,
        cwd: Option<String>,
        transcript: Option<String>,
        ppid: Option<i64>,
        term: Option<String>,
        term_session: Option<String>,
    }

    let aggs: Vec<Agg> = stmt
        .query_map([], |r| {
            Ok(Agg {
                session_id: r.get(0)?,
                first: r.get(1)?,
                last: r.get(2)?,
                prompt: r.get(3)?,
                stop: r.get(4)?,
                notif: r.get(5)?,
                end: r.get(6)?,
                tools: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                cwd: r.get(8)?,
                transcript: r.get(9)?,
                ppid: r.get(10)?,
                term: r.get(11)?,
                term_session: r.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    for a in aggs {
        // The last event *type* decides NEEDS_ACTION, so it is read from the
        // single newest row rather than an aggregate.
        let (last_type, last_notif_kind): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT event_type, notification_kind FROM events
                 WHERE session_id = ?1 ORDER BY ts DESC, id DESC LIMIT 1",
                [&a.session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((None, None));

        let end_reason: Option<String> = conn
            .query_row(
                "SELECT json_extract(raw,'$.reason') FROM events
                 WHERE session_id = ?1 AND event_type = 'SessionEnd'
                 ORDER BY ts DESC LIMIT 1",
                [&a.session_id],
                |r| r.get(0),
            )
            .unwrap_or(None);

        let existing = crate::store::load_session(conn, &a.session_id)?;
        let project_path = a
            .cwd
            .clone()
            .or_else(|| existing.as_ref().map(|e| e.project_path.clone()))
            .unwrap_or_default();
        if project_path.is_empty() {
            continue;
        }

        let p = |s: &Option<String>| s.as_deref().and_then(parse_ts);
        let incoming = Session {
            session_id: a.session_id.clone(),
            project_path: project_path.clone(),
            project_slug: existing
                .as_ref()
                .map(|e| e.project_slug.clone())
                .unwrap_or_else(|| crate::paths::slug_for_path(&project_path)),
            transcript_path: a.transcript.clone(),
            started_at: p(&a.first),
            last_event_at: p(&a.last),
            last_prompt_at: p(&a.prompt),
            last_stop_at: p(&a.stop),
            last_notif_at: p(&a.notif),
            last_notif_kind,
            last_event_type: last_type,
            ended_at: p(&a.end),
            end_reason,
            pid: a.ppid,
            term_program: a.term.clone(),
            term_session_id: a.term_session.clone(),
            tool_calls: a.tools,
            ..Default::default()
        };
        upsert_session(conn, existing.as_ref(), &incoming)?;
    }
    Ok(())
}

// --- projects --------------------------------------------------------------

/// Map every session to the git repos it actually edited files in.
///
/// The cwd recorded in a transcript is where `claude` was launched, which is
/// very often `$HOME` rather than a project. Grouping a week of work by that
/// collapses every repo into one non-repo bucket that can never have commits —
/// which silently guts the report. The files a session edited say where the
/// work really happened, so that is what defines its projects.
///
/// A session that touched three repos gets three rows, and appears under each
/// in the report with only that repo's files and commits.
fn resolve_session_projects(conn: &Connection, cfg: &Config) -> Result<()> {
    let sessions = crate::store::load_sessions(conn)?;
    conn.execute("DELETE FROM session_projects", [])?;

    let mut stmt = conn.prepare_cached(
        "INSERT INTO session_projects (session_id, project_path, edits) VALUES (?1,?2,?3)
         ON CONFLICT(session_id, project_path) DO UPDATE SET edits = edits + excluded.edits",
    )?;

    for s in &sessions {
        let files = crate::store::files_for(conn, &s.session_id)?;
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();

        for f in &files {
            let dir = Path::new(&f.path).parent().unwrap_or(Path::new("/"));
            if let Some(root) = git::repo_root(dir, cfg.git_timeout_secs) {
                let root = crate::paths::normalize(&root.display().to_string());
                if !cfg.is_excluded(&root) {
                    *counts.entry(root).or_insert(0) += f.edits;
                }
            }
        }

        // No edited file landed in a repo: fall back to the cwd so the session
        // still appears somewhere.
        if counts.is_empty() && !cfg.is_excluded(&s.project_path) {
            counts.insert(s.project_path.clone(), 0);
        }

        for (project, edits) in counts {
            stmt.execute(params![s.session_id, project, edits])?;
        }
    }
    Ok(())
}

// --- git -------------------------------------------------------------------

fn collect_git(conn: &Connection, cfg: &Config, stats: &mut IngestStats) -> Result<()> {
    let sessions = crate::store::load_sessions(conn)?;
    let links = crate::store::load_session_projects(conn)?;

    let mut by_project: BTreeMap<String, Vec<Session>> = BTreeMap::new();
    for s in sessions {
        let projects = links
            .get(&s.session_id)
            .cloned()
            .unwrap_or_else(|| vec![s.project_path.clone()]);
        for project in projects {
            if cfg.is_excluded(&project) {
                continue;
            }
            by_project.entry(project).or_default().push(s.clone());
        }
    }
    stats.projects = by_project.len();

    // Attribution is recomputed wholesale each run; it is cheap and this keeps
    // it consistent after a session's window grows.
    conn.execute("DELETE FROM session_commits", [])?;

    for (project_path, sessions) in &by_project {
        let dir = Path::new(project_path);
        let info = git::repo_info(dir, cfg.git_timeout_secs, cfg.git_cache_ttl_secs);
        if !info.is_repo {
            continue;
        }

        let earliest = sessions
            .iter()
            .filter_map(|s| s.started_at)
            .min()
            .unwrap_or_else(|| Utc::now() - Duration::days(cfg.git_lookback_days));

        let commits = git::log_since(
            dir,
            project_path,
            earliest - Duration::hours(1),
            info.author_email.as_deref(),
            info.branch.as_deref(),
            cfg.git_timeout_secs,
        );

        let mut stmt = conn.prepare_cached(
            "INSERT INTO commits (sha, project_path, ts, subject, author_email, branch)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(sha) DO UPDATE SET
                project_path=excluded.project_path, ts=excluded.ts, subject=excluded.subject,
                author_email=excluded.author_email, branch=excluded.branch",
        )?;
        for c in &commits {
            stmt.execute(params![
                c.sha,
                c.project_path,
                format_ts(&c.ts),
                c.subject,
                c.author_email,
                c.branch
            ])?;
        }
        stats.commits += commits.len();
        drop(stmt);

        stats.attributions += attribute(conn, cfg, sessions, &commits)?;
    }
    Ok(())
}

/// Attribute commits to sessions, then harvest ticket keys.
///
/// Git is ground truth for what shipped; the transcript is only evidence of
/// what was attempted. We never infer completion from transcript content.
fn attribute(
    conn: &Connection,
    cfg: &Config,
    sessions: &[Session],
    commits: &[Commit],
) -> Result<usize> {
    // Seed ticket keys from branch names before attribution, since a key match
    // is what promotes a commit to `exact`.
    let mut keys_by_session: HashMap<String, HashSet<String>> = sessions
        .iter()
        .map(|s| {
            let mut set: HashSet<String> = s.ticket_keys.iter().cloned().collect();
            if let Some(b) = &s.git_branch {
                set.extend(ticket_keys(b));
            }
            (s.session_id.clone(), set)
        })
        .collect();

    let grace = Duration::seconds(cfg.commit_grace_secs);
    let mut written = 0usize;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO session_commits (session_id, sha, confidence) VALUES (?1,?2,?3)
         ON CONFLICT(session_id, sha) DO UPDATE SET confidence = excluded.confidence",
    )?;

    for c in commits {
        let candidates: Vec<&Session> = sessions
            .iter()
            .filter(|s| {
                let started = match s.started_at {
                    Some(t) => t,
                    None => return false,
                };
                let last = s.last_event_at.unwrap_or(started);
                if c.ts < started || c.ts > last + grace {
                    return false;
                }
                // Only reject on a branch we actually know on both sides.
                !matches!(
                    (s.git_branch.as_deref(), c.branch.as_deref()),
                    (Some(a), Some(b)) if a != b && a != "HEAD" && b != "HEAD"
                )
            })
            .collect();

        if candidates.is_empty() {
            continue;
        }

        let subject_keys = ticket_keys(&c.subject);
        let ticket_matched: Vec<&&Session> = candidates
            .iter()
            .filter(|s| {
                keys_by_session
                    .get(&s.session_id)
                    .map(|k| k.iter().any(|key| subject_keys.contains(key)))
                    .unwrap_or(false)
            })
            .collect();

        let (winners, confidence): (Vec<&Session>, Confidence) = if !ticket_matched.is_empty() {
            (
                ticket_matched.into_iter().copied().collect(),
                Confidence::Exact,
            )
        } else if candidates.len() == 1 {
            (candidates.clone(), Confidence::Exact)
        } else {
            // Overlapping sessions on one branch: attribute to all and let the
            // human arbitrate from the rendered confidence. Do not guess.
            (candidates.clone(), Confidence::Window)
        };

        for s in winners {
            stmt.execute(params![s.session_id, c.sha, confidence.as_str()])?;
            written += 1;
            keys_by_session
                .entry(s.session_id.clone())
                .or_default()
                .extend(subject_keys.iter().cloned());
        }
    }
    drop(stmt);

    for (session_id, keys) in keys_by_session {
        let mut v: Vec<String> = keys.into_iter().collect();
        v.sort();
        conn.execute(
            "UPDATE sessions SET ticket_keys = ?1 WHERE session_id = ?2",
            params![serde_json::to_string(&v)?, session_id],
        )?;
    }
    Ok(written)
}

/// Prefixes that match the ticket-key shape but never name a ticket.
const NOT_TICKETS: &[&str] = &[
    "UTF", "ISO", "SHA", "RFC", "CVE", "HTTP", "IPV", "AES", "RSA", "SSE", "WCAG", "ES", "MD",
    "SHA1", "SHA256", "BASE", "UTC", "GMT", "X", "PR", "CI",
];

/// Extract ticket keys such as `ORCH-214`.
///
/// This is not issue-tracker integration. It is declining to throw away an
/// identifier the user already typed: no network calls, no API, no validation
/// that the key exists. The shape alone also matches things like `UTF-8`, so
/// well-known non-ticket prefixes are filtered out.
pub fn ticket_keys(text: &str) -> HashSet<String> {
    static PATTERN: &str = r"\b[A-Z][A-Z0-9]+-\d+\b";
    let re = match Regex::new(PATTERN) {
        Ok(r) => r,
        Err(_) => return HashSet::new(),
    };
    re.find_iter(text)
        .map(|m| m.as_str().to_string())
        .filter(|k| {
            let prefix = k.split('-').next().unwrap_or("");
            !NOT_TICKETS.contains(&prefix)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_paths_are_normalised_for_comparison() {
        // A session working under a symlinked directory records paths that do
        // not share a prefix with its own repo root, so every file-to-project
        // match silently misses. macOS `/tmp` -> `/private/tmp` is the common
        // case; this builds an explicit symlink so the test means the same
        // thing everywhere.
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("src")).unwrap();
        std::fs::write(real.join("src").join("cart.ts"), "x").unwrap();

        #[cfg(unix)]
        let link = {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            link
        };
        // No symlink to build on Windows; the normaliser still has to strip
        // the verbatim prefix, which is what this exercises there.
        #[cfg(not(unix))]
        let link = real.clone();

        let via_link = link.join("src").join("cart.ts");
        let resolved = crate::paths::normalize(via_link.to_str().unwrap());
        // Both sides through the same normaliser. Comparing against a raw
        // `canonicalize` would fail on Windows, where it keeps the `\\?\`
        // verbatim prefix that `normalize` strips.
        let root = crate::paths::normalize(real.to_str().unwrap());

        assert!(
            crate::paths::is_inside(&resolved, &root),
            "{resolved} should sit inside {root}"
        );
        assert_eq!(
            crate::paths::relative_within(&resolved, &root).as_deref(),
            Some("src/cart.ts")
        );
    }

    #[test]
    fn a_missing_file_keeps_the_path_it_was_recorded_with() {
        // Edited then deleted is normal; it must not vanish from the report.
        for p in ["/definitely/not/here/deleted.rs", r"C:\nope\gone.rs"] {
            assert_eq!(crate::paths::normalize(p), p);
        }
    }

    #[test]
    fn ticket_keys_extracts_real_keys() {
        let keys = ticket_keys("ORCH-214 fix pricing and close SA-9");
        assert!(keys.contains("ORCH-214"));
        assert!(keys.contains("SA-9"));
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn ticket_keys_ignores_lookalikes() {
        let keys = ticket_keys("switch encoding to UTF-8 and hash with SHA-256 per RFC-4180");
        assert!(keys.is_empty(), "got {keys:?}");
    }

    #[test]
    fn ticket_keys_from_branch_names() {
        assert!(ticket_keys("ORCH-214-fix-pricing").contains("ORCH-214"));
        assert!(ticket_keys("feature/no-key-here").is_empty());
    }

    #[test]
    fn merge_keeps_the_first_prompt_and_earliest_start() {
        let conn = db::open_memory().unwrap();
        let old = Session {
            session_id: "s".into(),
            project_path: "/p".into(),
            project_slug: "-p".into(),
            first_prompt: Some("the original ask".into()),
            started_at: parse_ts("2026-07-01T00:00:00.000Z"),
            last_event_at: parse_ts("2026-07-01T01:00:00.000Z"),
            ..Default::default()
        };
        write_session(&conn, &old).unwrap();

        let incoming = Session {
            session_id: "s".into(),
            project_path: "/p".into(),
            project_slug: "-p".into(),
            first_prompt: Some("a later prompt".into()),
            summary: Some("New title".into()),
            started_at: parse_ts("2026-06-30T00:00:00.000Z"),
            last_event_at: parse_ts("2026-07-02T00:00:00.000Z"),
            ..Default::default()
        };
        upsert_session(&conn, Some(&old), &incoming).unwrap();

        let got = crate::store::load_session(&conn, "s").unwrap().unwrap();
        assert_eq!(got.first_prompt.as_deref(), Some("the original ask"));
        assert_eq!(got.summary.as_deref(), Some("New title"));
        assert_eq!(got.started_at, parse_ts("2026-06-30T00:00:00.000Z"));
        assert_eq!(got.last_event_at, parse_ts("2026-07-02T00:00:00.000Z"));
    }

    #[test]
    fn rollup_reads_last_event_type_from_the_newest_row() {
        let conn = db::open_memory().unwrap();
        let insert = |kind: &str, ts: &str, notif: Option<&str>| {
            conn.execute(
                "INSERT INTO events (session_id, event_type, ts, cwd, notification_kind, raw)
                 VALUES ('s', ?1, ?2, '/p', ?3, '{}')",
                params![kind, ts, notif],
            )
            .unwrap();
        };
        insert("SessionStart", "2026-07-24T10:00:00.000Z", None);
        insert("UserPromptSubmit", "2026-07-24T10:01:00.000Z", None);
        insert("PostToolUse", "2026-07-24T10:02:00.000Z", None);
        insert(
            "Notification",
            "2026-07-24T10:03:00.000Z",
            Some("permission_prompt"),
        );

        rollup_events(&conn).unwrap();
        let s = crate::store::load_session(&conn, "s").unwrap().unwrap();
        assert_eq!(s.last_event_type.as_deref(), Some("Notification"));
        assert_eq!(s.last_notif_kind.as_deref(), Some("permission_prompt"));
        assert_eq!(s.last_prompt_at, parse_ts("2026-07-24T10:01:00.000Z"));
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.project_path, "/p");
    }

    #[test]
    fn session_end_makes_the_session_ended() {
        let conn = db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO events (session_id, event_type, ts, cwd, raw)
             VALUES ('s','SessionEnd','2026-07-24T10:00:00.000Z','/p','{\"reason\":\"clear\"}')",
            [],
        )
        .unwrap();
        rollup_events(&conn).unwrap();
        let s = crate::store::load_session(&conn, "s").unwrap().unwrap();
        assert!(s.ended_at.is_some());
        assert_eq!(s.end_reason.as_deref(), Some("clear"));
    }

    #[test]
    fn a_lone_session_on_a_branch_gets_exact_attribution() {
        let conn = db::open_memory().unwrap();
        let cfg = Config::default();
        let s = Session {
            session_id: "s1".into(),
            project_path: "/p".into(),
            project_slug: "-p".into(),
            git_branch: Some("main".into()),
            started_at: parse_ts("2026-07-24T09:00:00.000Z"),
            last_event_at: parse_ts("2026-07-24T11:00:00.000Z"),
            ..Default::default()
        };
        write_session(&conn, &s).unwrap();
        conn.execute(
            "INSERT INTO commits (sha, project_path, ts, subject, branch)
             VALUES ('abc','/p','2026-07-24T10:00:00.000Z','do a thing','main')",
            [],
        )
        .unwrap();

        let commits = vec![Commit {
            sha: "abc".into(),
            project_path: "/p".into(),
            ts: parse_ts("2026-07-24T10:00:00.000Z").unwrap(),
            subject: "do a thing".into(),
            author_email: None,
            branch: Some("main".into()),
        }];
        assert_eq!(attribute(&conn, &cfg, &[s], &commits).unwrap(), 1);

        let got = crate::store::commits_for(&conn, "s1").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].confidence, Confidence::Exact);
    }

    #[test]
    fn overlapping_sessions_are_attributed_to_both_as_window() {
        let conn = db::open_memory().unwrap();
        let cfg = Config::default();
        let mk = |id: &str| Session {
            session_id: id.into(),
            project_path: "/p".into(),
            project_slug: "-p".into(),
            git_branch: Some("main".into()),
            started_at: parse_ts("2026-07-24T09:00:00.000Z"),
            last_event_at: parse_ts("2026-07-24T11:00:00.000Z"),
            ..Default::default()
        };
        let sessions = vec![mk("s1"), mk("s2")];
        for s in &sessions {
            write_session(&conn, s).unwrap();
        }
        conn.execute(
            "INSERT INTO commits (sha, project_path, ts, subject, branch)
             VALUES ('abc','/p','2026-07-24T10:00:00.000Z','shared work','main')",
            [],
        )
        .unwrap();

        let commits = vec![Commit {
            sha: "abc".into(),
            project_path: "/p".into(),
            ts: parse_ts("2026-07-24T10:00:00.000Z").unwrap(),
            subject: "shared work".into(),
            author_email: None,
            branch: Some("main".into()),
        }];
        assert_eq!(attribute(&conn, &cfg, &sessions, &commits).unwrap(), 2);
        assert_eq!(
            crate::store::commits_for(&conn, "s1").unwrap()[0].confidence,
            Confidence::Window
        );
        assert_eq!(
            crate::store::commits_for(&conn, "s2").unwrap()[0].confidence,
            Confidence::Window
        );
    }

    #[test]
    fn a_ticket_key_match_beats_overlap_and_stays_exact() {
        let conn = db::open_memory().unwrap();
        let cfg = Config::default();
        let mk = |id: &str, branch: &str| Session {
            session_id: id.into(),
            project_path: "/p".into(),
            project_slug: "-p".into(),
            git_branch: Some(branch.into()),
            started_at: parse_ts("2026-07-24T09:00:00.000Z"),
            last_event_at: parse_ts("2026-07-24T11:00:00.000Z"),
            ..Default::default()
        };
        let sessions = vec![mk("s1", "ORCH-214-pricing"), mk("s2", "ORCH-214-pricing")];
        for s in &sessions {
            write_session(&conn, s).unwrap();
        }
        let commits = vec![Commit {
            sha: "abc".into(),
            project_path: "/p".into(),
            ts: parse_ts("2026-07-24T10:00:00.000Z").unwrap(),
            subject: "ORCH-214 fix pricing tier copy".into(),
            author_email: None,
            branch: Some("ORCH-214-pricing".into()),
        }];
        conn.execute(
            "INSERT INTO commits (sha, project_path, ts, subject, branch)
             VALUES ('abc','/p','2026-07-24T10:00:00.000Z','ORCH-214 fix pricing tier copy','ORCH-214-pricing')",
            [],
        )
        .unwrap();

        attribute(&conn, &cfg, &sessions, &commits).unwrap();
        let got = crate::store::commits_for(&conn, "s1").unwrap();
        assert_eq!(got[0].confidence, Confidence::Exact);

        let s = crate::store::load_session(&conn, "s1").unwrap().unwrap();
        assert!(s.ticket_keys.contains(&"ORCH-214".to_string()));
    }

    #[test]
    fn commits_outside_every_window_are_not_attributed() {
        let conn = db::open_memory().unwrap();
        let cfg = Config::default();
        let s = Session {
            session_id: "s1".into(),
            project_path: "/p".into(),
            project_slug: "-p".into(),
            started_at: parse_ts("2026-07-24T09:00:00.000Z"),
            last_event_at: parse_ts("2026-07-24T10:00:00.000Z"),
            ..Default::default()
        };
        write_session(&conn, &s).unwrap();
        let commits = vec![Commit {
            sha: "old".into(),
            project_path: "/p".into(),
            // Well past the window plus the grace period.
            ts: parse_ts("2026-07-24T14:00:00.000Z").unwrap(),
            subject: "much later".into(),
            author_email: None,
            branch: None,
        }];
        assert_eq!(attribute(&conn, &cfg, &[s], &commits).unwrap(), 0);
    }
}
