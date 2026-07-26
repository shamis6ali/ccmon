//! SQLite storage. `events` is the source of truth and is append-only;
//! everything else is derived and must be rebuildable by `reindex --force`.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);

-- Append-only mirror of the spool. Never updated, never deleted from.
CREATE TABLE IF NOT EXISTS events (
  id                INTEGER PRIMARY KEY,
  session_id        TEXT NOT NULL,
  event_type        TEXT NOT NULL,
  ts                TEXT NOT NULL,
  cwd               TEXT,
  transcript_path   TEXT,
  permission_mode   TEXT,
  notification_kind TEXT,
  tool_name         TEXT,
  ppid              INTEGER,
  term_program      TEXT,
  term_session_id   TEXT,
  raw               TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_session_ts ON events(session_id, ts);
CREATE INDEX IF NOT EXISTS idx_events_ts         ON events(ts);

CREATE TABLE IF NOT EXISTS sessions (
  session_id      TEXT PRIMARY KEY,
  project_path    TEXT NOT NULL,
  project_slug    TEXT NOT NULL,
  transcript_path TEXT,
  summary         TEXT,
  first_prompt    TEXT,
  git_branch      TEXT,
  source          TEXT,
  started_at      TEXT,
  last_event_at   TEXT,
  last_prompt_at  TEXT,
  last_stop_at    TEXT,
  last_notif_at   TEXT,
  last_notif_kind TEXT,
  last_event_type TEXT,
  ended_at        TEXT,
  end_reason      TEXT,
  pid             INTEGER,
  term_program    TEXT,
  term_session_id TEXT,
  tool_calls      INTEGER NOT NULL DEFAULT 0,
  ticket_keys     TEXT,
  -- Snapshot of Claude Code's own runtime file for this session.
  runtime_status  TEXT,
  waiting_for     TEXT,
  proc_start      TEXT,
  session_name    TEXT,
  runtime_kind    TEXT
);
CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_path);
CREATE INDEX IF NOT EXISTS idx_sessions_last    ON sessions(last_event_at);

CREATE TABLE IF NOT EXISTS session_files (
  session_id TEXT NOT NULL,
  path       TEXT NOT NULL,
  edits      INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (session_id, path)
);

-- Which git repos a session actually worked in, derived from the files it
-- edited. A session's cwd is frequently not the project: running `claude` from
-- $HOME and editing files across several repos is normal, and grouping by cwd
-- would collapse all of that into one non-repo bucket with no commits.
CREATE TABLE IF NOT EXISTS session_projects (
  session_id   TEXT NOT NULL,
  project_path TEXT NOT NULL,
  edits        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, project_path)
);
CREATE INDEX IF NOT EXISTS idx_session_projects_project ON session_projects(project_path);

CREATE TABLE IF NOT EXISTS session_todos (
  session_id TEXT NOT NULL,
  content    TEXT NOT NULL,
  status     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_todos_session ON session_todos(session_id);

CREATE TABLE IF NOT EXISTS commits (
  sha          TEXT PRIMARY KEY,
  project_path TEXT NOT NULL,
  ts           TEXT NOT NULL,
  subject      TEXT NOT NULL,
  author_email TEXT,
  branch       TEXT
);
CREATE INDEX IF NOT EXISTS idx_commits_project_ts ON commits(project_path, ts);

CREATE TABLE IF NOT EXISTS session_commits (
  session_id TEXT NOT NULL,
  sha        TEXT NOT NULL,
  confidence TEXT NOT NULL,
  PRIMARY KEY (session_id, sha)
);

CREATE TABLE IF NOT EXISTS ingest_state (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

/// Open the default database, applying pragmas and migrations.
pub fn open_default() -> Result<Connection> {
    let path = crate::paths::db_path()?;
    open(&path)
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(path)
        .with_context(|| format!("opening database at {}", path.display()))?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// In-memory database, for tests.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // WAL plus a busy timeout is what makes concurrent readers safe without a
    // daemon: the CLI, the app, and the MCP server all ingest on demand.
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "busy_timeout", 3000)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL").ok();
    Ok(())
}

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("applying schema")?;
    let current: Option<i64> = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
            r.get(0)
        })
        .ok();
    match current {
        None => {
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                [SCHEMA_VERSION],
            )?;
        }
        Some(v) if v < SCHEMA_VERSION => {
            // Derived tables are disposable, so forward migration is just a
            // rebuild. Only `events` carries data we cannot recreate.
            conn.execute("UPDATE schema_version SET version = ?1", [SCHEMA_VERSION])?;
            clear_derived(conn)?;
            clear_ingest_state_prefix(conn, "transcript_mtime:")?;
        }
        Some(_) => {}
    }
    Ok(())
}

/// Drop every derived row. `events` and spool offsets survive.
pub fn clear_derived(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM sessions;
         DELETE FROM session_files;
         DELETE FROM session_projects;
         DELETE FROM session_todos;
         DELETE FROM session_commits;
         DELETE FROM commits;",
    )?;
    Ok(())
}

pub fn clear_ingest_state_prefix(conn: &Connection, prefix: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ingest_state WHERE key LIKE ?1 || '%'",
        [prefix],
    )?;
    Ok(())
}

pub fn get_state(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM ingest_state WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .ok()
}

pub fn set_state(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO ingest_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent() {
        let conn = open_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ingest_state_upserts() {
        let conn = open_memory().unwrap();
        set_state(&conn, "k", "1").unwrap();
        set_state(&conn, "k", "2").unwrap();
        assert_eq!(get_state(&conn, "k").as_deref(), Some("2"));
        assert_eq!(get_state(&conn, "missing"), None);
    }

    #[test]
    fn clear_derived_keeps_events() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO events (session_id, event_type, ts, raw) VALUES ('s','Stop','2026-01-01T00:00:00.000Z','{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (session_id, project_path, project_slug) VALUES ('s','/p','-p')",
            [],
        )
        .unwrap();
        clear_derived(&conn).unwrap();
        let events: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        let sessions: i64 = conn
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 1);
        assert_eq!(sessions, 0);
    }
}
