//! Live session runtime state from `<claude-root>/sessions/<pid>.json`.
//!
//! The spec assumed pid, process start time, and the needs-action signal could
//! only be captured by a hook at the moment it fired. They cannot be recovered
//! from a transcript — but Claude Code writes them itself, per running process:
//!
//! ```json
//! {"pid":32575,"sessionId":"0db4c0e3-…","cwd":"/Users/x","startedAt":1783714023142,
//!  "procStart":"Fri Jul 10 20:07:01 2026","kind":"interactive","name":"worker-queue",
//!  "status":"waiting","waitingFor":"permission prompt","updatedAt":1784837632344}
//! ```
//!
//! That gives live triage with no hooks installed at all, which is why M1 is
//! useful on its own. Hooks remain worth installing (M2): these files record
//! *current* state only, while the spool records turn boundaries, failures, and
//! history that no snapshot can reconstruct.
//!
//! Like everything else Claude Code writes, this is an undocumented format —
//! parsed permissively, and its absence is never an error.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuntimeSession {
    pub pid: Option<i64>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// Epoch milliseconds.
    #[serde(rename = "startedAt")]
    pub started_at: Option<i64>,
    /// Process start time as ctime text, e.g. `Fri Jul 10 20:07:01 2026`.
    /// Local time. This is the PID-reuse guard.
    #[serde(rename = "procStart")]
    pub proc_start: Option<String>,
    pub version: Option<String>,
    /// `interactive` or `bg`.
    pub kind: Option<String>,
    pub entrypoint: Option<String>,
    /// Human-facing session name.
    pub name: Option<String>,
    /// `idle` | `busy` | `waiting`.
    pub status: Option<String>,
    /// Set when `status == "waiting"`, e.g. `permission prompt`.
    #[serde(rename = "waitingFor")]
    pub waiting_for: Option<String>,
    /// Epoch milliseconds.
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<i64>,
    #[serde(rename = "statusUpdatedAt")]
    pub status_updated_at: Option<i64>,
    #[serde(flatten)]
    #[allow(dead_code)]
    pub extra: Map<String, Value>,
}

impl RuntimeSession {
    pub fn started_at_utc(&self) -> Option<DateTime<Utc>> {
        self.started_at.and_then(from_epoch_ms)
    }
    pub fn updated_at_utc(&self) -> Option<DateTime<Utc>> {
        self.updated_at.and_then(from_epoch_ms)
    }
    pub fn status_updated_at_utc(&self) -> Option<DateTime<Utc>> {
        self.status_updated_at.and_then(from_epoch_ms)
    }
    pub fn is_background(&self) -> bool {
        self.kind.as_deref() == Some("bg")
    }
}

fn from_epoch_ms(ms: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms).single()
}

/// Read every runtime file under a Claude root's `sessions/` dir.
///
/// When several pid files name the same session (a resume reuses the session id
/// under a new pid), the most recently updated wins.
pub fn scan(sessions_dir: &Path) -> Vec<RuntimeSession> {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut best: Vec<RuntimeSession> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "unreadable runtime file");
                continue;
            }
        };
        let rs: RuntimeSession = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "skipping runtime file");
                continue;
            }
        };
        if rs.session_id.is_none() {
            continue;
        }

        match best.iter_mut().find(|b| b.session_id == rs.session_id) {
            Some(existing) => {
                if rs.updated_at.unwrap_or(0) > existing.updated_at.unwrap_or(0) {
                    *existing = rs;
                }
            }
            None => best.push(rs),
        }
    }
    best
}

/// Parse Claude Code's ctime-style `procStart`.
///
/// **The timestamp is UTC**, despite looking exactly like local `ctime` output.
/// Verified against `ps -o lstart=`: a file reading `Mon Jul 20 15:38:07 2026`
/// belongs to a process `ps` reports as starting `Mon 20 Jul 09:38:07 2026` in
/// UTC-6. Reading it as local time shifts every process start by the UTC
/// offset, which makes the PID-reuse guard reject every live session and
/// report the user's entire fleet as DEAD.
///
/// Returns `None` on any deviation from the expected shape; the caller treats
/// an unparseable start time as "unknown", never as a mismatch.
pub fn parse_proc_start(s: &str) -> Option<DateTime<Utc>> {
    use chrono::NaiveDateTime;
    // `%e` is the space-padded day, which is what ctime emits ("Jul  3").
    let naive = NaiveDateTime::parse_from_str(s.trim(), "%a %b %e %H:%M:%S %Y").ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_runtime_file() {
        let json = r#"{"pid":93962,"sessionId":"aecf0449-24ff-4cb2-a045-a8ca168ebc30","cwd":"/home/dev","startedAt":1784561891870,"procStart":"Mon Jul 20 15:38:07 2026","version":"2.1.211","peerProtocol":1,"kind":"interactive","entrypoint":"cli","name":"queue-migration","status":"idle","updatedAt":1784650834045,"statusUpdatedAt":1784650834045,"bridgeSessionId":"session_01Dwt"}"#;
        let rs: RuntimeSession = serde_json::from_str(json).unwrap();
        assert_eq!(rs.pid, Some(93962));
        assert_eq!(rs.status.as_deref(), Some("idle"));
        assert_eq!(rs.name.as_deref(), Some("queue-migration"));
        assert!(!rs.is_background());
        assert!(rs.started_at_utc().is_some());
    }

    #[test]
    fn parses_waiting_state_with_reason() {
        let json = r#"{"pid":32575,"sessionId":"s","status":"waiting","waitingFor":"permission prompt","procStart":"Fri Jul 10 20:07:01 2026"}"#;
        let rs: RuntimeSession = serde_json::from_str(json).unwrap();
        assert_eq!(rs.status.as_deref(), Some("waiting"));
        assert_eq!(rs.waiting_for.as_deref(), Some("permission prompt"));
    }

    #[test]
    fn unknown_future_fields_are_kept_not_rejected() {
        let json = r#"{"pid":1,"sessionId":"s","somethingNew":{"a":1},"status":"busy"}"#;
        let rs: RuntimeSession = serde_json::from_str(json).unwrap();
        assert_eq!(rs.status.as_deref(), Some("busy"));
        assert!(rs.extra.contains_key("somethingNew"));
    }

    #[test]
    fn proc_start_handles_space_padded_days() {
        assert!(parse_proc_start("Fri Jul  3 16:21:01 2026").is_some());
        assert!(parse_proc_start("Mon Jul 20 15:38:07 2026").is_some());
        assert!(parse_proc_start("garbage").is_none());
        assert!(parse_proc_start("").is_none());
    }

    #[test]
    fn proc_start_is_utc_not_local() {
        // Regression guard. Reading this as local time shifts the value by the
        // UTC offset, fails the PID-reuse guard on every live session, and
        // reports the whole fleet as DEAD.
        let parsed = parse_proc_start("Mon Jul 20 15:38:07 2026").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-07-20T15:38:07+00:00");
    }

    #[test]
    fn missing_directory_yields_nothing() {
        assert!(scan(Path::new("/nonexistent/ccmon/sessions")).is_empty());
    }

    #[test]
    fn newest_file_wins_for_a_repeated_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("1.json"),
            r#"{"pid":1,"sessionId":"same","status":"idle","updatedAt":100}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("2.json"),
            r#"{"pid":2,"sessionId":"same","status":"busy","updatedAt":200}"#,
        )
        .unwrap();
        let found = scan(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, Some(2));
        assert_eq!(found[0].status.as_deref(), Some("busy"));
    }

    #[test]
    fn malformed_files_are_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("bad.json"), "{not json").unwrap();
        std::fs::write(tmp.path().join("no-id.json"), r#"{"pid":5}"#).unwrap();
        std::fs::write(tmp.path().join("ignored.txt"), "whatever").unwrap();
        std::fs::write(
            tmp.path().join("ok.json"),
            r#"{"pid":7,"sessionId":"s","status":"idle"}"#,
        )
        .unwrap();
        let found = scan(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, Some(7));
    }
}
