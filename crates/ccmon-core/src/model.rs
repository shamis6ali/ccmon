//! Shared types. Timestamps are UTC everywhere internally; local time appears
//! only at render.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// Derived session state. Evaluated in order; first match wins. See `state.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionState {
    Ended,
    Dead,
    NeedsAction,
    Working,
    NeedsReview,
    Idle,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Ended => "ENDED",
            SessionState::Dead => "DEAD",
            SessionState::NeedsAction => "NEEDS_ACTION",
            SessionState::Working => "WORKING",
            SessionState::NeedsReview => "NEEDS_REVIEW",
            SessionState::Idle => "IDLE",
        }
    }

    /// Sort key for grouping in the UI and `ccmon ls`: most urgent first.
    pub fn rank(&self) -> u8 {
        match self {
            SessionState::NeedsAction => 0,
            SessionState::Working => 1,
            SessionState::NeedsReview => 2,
            SessionState::Idle => 3,
            SessionState::Dead => 4,
            SessionState::Ended => 5,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_uppercase().replace('-', "_").as_str() {
            "ENDED" => SessionState::Ended,
            "DEAD" => SessionState::Dead,
            "NEEDS_ACTION" => SessionState::NeedsAction,
            "WORKING" => SessionState::Working,
            "NEEDS_REVIEW" => SessionState::NeedsReview,
            "IDLE" => SessionState::Idle,
            _ => return None,
        })
    }
}

/// Why a session needs the user. Carried alongside `NEEDS_ACTION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    PermissionPrompt,
    IdlePrompt,
    StopFailure,
    /// An open turn that stopped emitting events: Claude Code died mid-turn
    /// without the process going away, or is stuck on a network call.
    StalledTurn,
}

impl ActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::PermissionPrompt => "permission_prompt",
            ActionKind::IdlePrompt => "idle_prompt",
            ActionKind::StopFailure => "stop_failure",
            ActionKind::StalledTurn => "stalled_turn",
        }
    }
}

/// Process liveness. `Unknown` is a real answer, not a failure: sessions
/// backfilled from transcripts alone never had a hook capture their pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

/// A row of the derived `sessions` rollup. Fully rebuildable from `events`
/// plus transcripts by `ccmon reindex --force`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub project_path: String,
    pub project_slug: String,
    pub transcript_path: Option<String>,
    /// Matches the string Claude Code writes into the terminal window title,
    /// which is what lets the user find the window themselves.
    pub summary: Option<String>,
    pub first_prompt: Option<String>,
    pub git_branch: Option<String>,
    pub source: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub last_prompt_at: Option<DateTime<Utc>>,
    pub last_stop_at: Option<DateTime<Utc>>,
    pub last_notif_at: Option<DateTime<Utc>>,
    pub last_notif_kind: Option<String>,
    pub last_event_type: Option<String>,
    pub ended_at: Option<DateTime<Utc>>,
    pub end_reason: Option<String>,
    pub pid: Option<i64>,
    pub term_program: Option<String>,
    pub term_session_id: Option<String>,
    pub tool_calls: i64,
    pub ticket_keys: Vec<String>,

    // --- from Claude Code's own runtime files (`sessions/<pid>.json`) ---
    /// `idle` | `busy` | `waiting`, as Claude Code reports it right now.
    pub runtime_status: Option<String>,
    /// Why it is waiting, e.g. `permission prompt`.
    pub waiting_for: Option<String>,
    /// When the process started, for the PID-reuse guard.
    pub proc_start: Option<DateTime<Utc>>,
    /// Claude Code's own name for the session.
    pub session_name: Option<String>,
    /// `interactive` or `bg`.
    pub runtime_kind: Option<String>,
}

impl Session {
    /// The best human-facing label: the AI title if there is one, else Claude
    /// Code's session name, else the session id.
    pub fn display_title(&self) -> String {
        self.summary
            .clone()
            .or_else(|| self.session_name.clone())
            .unwrap_or_else(|| self.session_id.clone())
    }

    pub fn is_background(&self) -> bool {
        self.runtime_kind.as_deref() == Some("bg")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub project_path: String,
    pub ts: DateTime<Utc>,
    pub subject: String,
    pub author_email: Option<String>,
    pub branch: Option<String>,
}

/// How confident we are that a commit belongs to a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// A ticket key matched, or exactly one session owned the branch at that time.
    Exact,
    /// Several sessions overlapped on the branch. Attributed to all of them;
    /// the report renders the confidence so the human can arbitrate.
    Window,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Window => "window",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "exact" => Confidence::Exact,
            _ => Confidence::Window,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "completed" => TodoStatus::Completed,
            "in_progress" => TodoStatus::InProgress,
            _ => TodoStatus::Pending,
        }
    }
    /// Pending and in-progress both mean "unfinished work is on the table".
    pub fn is_open(&self) -> bool {
        !matches!(self, TodoStatus::Completed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: TodoStatus,
}

/// A session plus everything derived at read time: state, liveness, git.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    #[serde(flatten)]
    pub session: Session,
    pub state: SessionState,
    pub stale: bool,
    pub action_kind: Option<ActionKind>,
    pub liveness: Liveness,
    /// `None` when the project is not a git repo or git timed out.
    pub worktree_dirty: Option<bool>,
    pub open_todos: i64,
    pub files: Vec<FileEdit>,
    pub commits: Vec<AttributedCommit>,
    /// Repos this session actually edited files in, most-edited first. Usually
    /// *not* the same as `session.project_path`, which is only where `claude`
    /// happened to be launched.
    pub projects: Vec<ProjectEdits>,
}

impl SessionView {
    /// The repo where most of this session's work happened, falling back to
    /// the cwd when nothing landed in a repo.
    pub fn primary_project(&self) -> &str {
        self.projects
            .first()
            .map(|p| p.project_path.as_str())
            .unwrap_or(self.session.project_path.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEdits {
    pub project_path: String,
    pub edits: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: String,
    pub edits: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributedCommit {
    #[serde(flatten)]
    pub commit: Commit,
    pub confidence: Confidence,
}

/// The canonical on-the-wire timestamp format: ISO 8601, UTC, milliseconds.
pub fn format_ts(ts: &DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Permissive timestamp parsing. Returns `None` rather than erroring, because
/// no single bad field in a transcript may fail a run.
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// `a` if it is later than `b`, keeping whichever is present.
pub fn max_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

pub fn min_opt<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_ranking_puts_needs_action_first() {
        let mut v = [
            SessionState::Idle,
            SessionState::NeedsAction,
            SessionState::Working,
            SessionState::Ended,
        ];
        v.sort_by_key(|s| s.rank());
        assert_eq!(v[0], SessionState::NeedsAction);
        assert_eq!(v[1], SessionState::Working);
    }

    #[test]
    fn timestamps_round_trip() {
        let s = "2026-07-24T21:44:51.123Z";
        let t = parse_ts(s).unwrap();
        assert_eq!(format_ts(&t), s);
    }

    #[test]
    fn timestamp_parsing_tolerates_offsets_and_rejects_garbage() {
        assert!(parse_ts("2026-07-24T21:44:51-06:00").is_some());
        assert!(parse_ts("not a timestamp").is_none());
        assert!(parse_ts("").is_none());
    }

    #[test]
    fn max_opt_prefers_present_and_later() {
        assert_eq!(max_opt(Some(3), Some(5)), Some(5));
        assert_eq!(max_opt(Some(3), None), Some(3));
        assert_eq!(max_opt(None, Some(5)), Some(5));
        assert_eq!(max_opt::<i32>(None, None), None);
    }
}
