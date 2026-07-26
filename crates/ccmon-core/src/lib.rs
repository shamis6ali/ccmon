//! ccmon core: read Claude Code's own on-disk artifacts, derive a state per
//! session, and emit a work report.
//!
//! **There is no daemon.** The hook appends one NDJSON line and exits; readers
//! (CLI, desktop app, MCP server) drain that spool into SQLite on demand before
//! answering. Ingest is idempotent and tracks a byte offset, so concurrent
//! readers converge safely. Staleness is a read-time computation rather than
//! something a timer has to sweep.
//!
//! ccmon is strictly read-only with respect to Claude Code's data. The single
//! exception is `settings.json` during `ccmon install`, which is backed up
//! first. It makes no network calls and never invokes an LLM: the report is
//! deterministic and mechanically generated, so summarization happens in the
//! user's chat rather than in the tool.

pub mod config;
pub mod db;
pub mod git;
pub mod ingest;
pub mod model;
pub mod paths;
pub mod redact;
pub mod report;
pub mod runtime;
pub mod spool;
pub mod state;
pub mod store;
pub mod todos;
pub mod transcript;

/// Re-exported so front ends can hold a `Connection` without depending on a
/// matching rusqlite version themselves.
pub use rusqlite;

pub use config::Config;
pub use model::{
    ActionKind, Confidence, Liveness, Session, SessionState, SessionView, Todo, TodoStatus,
};

use anyhow::Result;
use rusqlite::Connection;

/// Open the database, ingest everything, and return fully derived views.
///
/// This is the one call every front end makes.
pub fn refresh(conn: &Connection, cfg: &Config) -> Result<Vec<SessionView>> {
    ingest::run(conn, cfg)?;
    store::build_views(conn, cfg, chrono::Utc::now())
}
