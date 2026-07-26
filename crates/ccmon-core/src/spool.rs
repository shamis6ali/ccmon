//! The NDJSON event spool: `events.jsonl` plus rotated `events.1.jsonl` and
//! `events.2.jsonl`.
//!
//! There is no daemon. The hook appends one line and exits; readers drain the
//! spool into SQLite on demand before answering. Ingest tracks
//! `(file_identity, byte_offset)` — inode on Unix, creation time on Windows —
//! so rotation never causes a re-ingest or a skipped event.

use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Current spool schema version. Bump on a breaking change; readers must
/// support every old version forever.
pub const SPOOL_VERSION: u32 = 1;

/// Keep lines comfortably under `PIPE_BUF` so concurrent appends stay atomic.
/// POSIX guarantees 512 bytes; Linux gives 4096. We target 4 KB and truncate
/// the passthrough payload to stay inside it.
pub const MAX_LINE_BYTES: usize = 4096;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpoolEvent {
    pub v: u32,
    pub ts: String,
    pub event: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ppid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub term_program: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub term_session_id: Option<String>,
    /// Unknown fields are carried, never rejected — the hook event set is
    /// actively expanding upstream.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Append one line to the spool with a single `write` under `O_APPEND`.
///
/// Used by `ccmon-hook`, which runs synchronously inside the agent loop and
/// must never fail loudly: every error path here is the caller's cue to exit 0
/// silently. Losing a monitoring event is acceptable; interfering with a
/// Claude Code session is not.
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    if buf.len() > MAX_LINE_BYTES {
        // Truncating is better than a partial write that another reader would
        // see interleaved.
        buf.truncate(MAX_LINE_BYTES - 1);
        buf.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(buf.as_bytes())
}

/// The spool plus its rotated siblings, newest first.
pub fn spool_files(primary: &Path) -> Vec<PathBuf> {
    let mut v = vec![primary.to_path_buf()];
    let stem = primary
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "events".into());
    let dir = primary.parent().map(Path::to_path_buf).unwrap_or_default();
    for n in 1..=2 {
        v.push(dir.join(format!("{stem}.{n}.jsonl")));
    }
    v
}

/// Rotate when the spool exceeds `max_bytes`, keeping two generations.
pub fn rotate_if_needed(primary: &Path, max_bytes: u64) -> Result<bool> {
    let len = match std::fs::metadata(primary) {
        Ok(m) => m.len(),
        Err(_) => return Ok(false),
    };
    if len <= max_bytes {
        return Ok(false);
    }
    let files = spool_files(primary);
    // events.1 -> events.2, dropping the old events.2.
    if files[2].exists() {
        std::fs::remove_file(&files[2])?;
    }
    if files[1].exists() {
        std::fs::rename(&files[1], &files[2])?;
    }
    std::fs::rename(&files[0], &files[1])?;
    Ok(true)
}

/// Stable-enough identity for a spool file across rotations.
fn file_identity(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let _ = &meta;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!("ino:{}", meta.ino()))
    }

    // Not Unix: identify the file by its first line instead.
    //
    // The obvious Windows answer, creation time, is wrong. NTFS *file system
    // tunneling* gives a newly created file the creation time of a file that
    // was renamed out of the way moments earlier — which is exactly what
    // rotation does. The rotated file and its replacement then share one
    // identity, clobber each other's stored offset, and the whole spool is
    // re-ingested. (`file_index` would be ideal but is still unstable.)
    //
    // A spool's first line never changes once written, and every event carries
    // a timestamp and session id, so it distinguishes files reliably and
    // survives the rename.
    #[cfg(not(unix))]
    {
        use std::io::Read;
        let mut file = std::fs::File::open(path).ok()?;
        let mut buf = [0u8; 4096];
        let read = file.read(&mut buf).ok()?;
        let line_end = buf[..read].iter().position(|b| *b == b'\n')?;
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in &buf[..line_end] {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Some(format!("line0:{hash:016x}"))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DrainStats {
    pub lines_read: usize,
    pub events_inserted: usize,
    pub lines_skipped: usize,
}

/// Drain every spool file into `events` from its stored offset.
///
/// Idempotent: safe to run concurrently from the CLI, the app, and the MCP
/// server at once.
pub fn drain(conn: &Connection, primary: &Path) -> Result<DrainStats> {
    let mut stats = DrainStats::default();
    for path in spool_files(primary) {
        if !path.exists() {
            continue;
        }
        stats_add(&mut stats, drain_one(conn, &path)?);
    }
    Ok(stats)
}

fn stats_add(acc: &mut DrainStats, s: DrainStats) {
    acc.lines_read += s.lines_read;
    acc.events_inserted += s.events_inserted;
    acc.lines_skipped += s.lines_skipped;
}

/// Longest prefix we fingerprint.
const FINGERPRINT_BYTES: u64 = 256;

/// FNV-1a over the first `n` bytes of a file.
///
/// `n` is always tied to bytes we have *already consumed*, never to the
/// current length — otherwise a growing file would change its own fingerprint
/// on every append and look like a rewrite.
///
/// Appending never changes this; truncating and rewriting in place does.
/// Length alone cannot tell those apart, because a spool that is emptied and
/// refilled to a similar size would otherwise look like "nothing new".
fn head_fingerprint(path: &Path, n: u64) -> u64 {
    use std::io::Read;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    if n == 0 {
        return hash;
    }
    let mut buf = vec![0u8; n.min(FINGERPRINT_BYTES) as usize];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    for b in &buf[..read] {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn drain_one(conn: &Connection, path: &Path) -> Result<DrainStats> {
    let mut stats = DrainStats::default();
    let identity = match file_identity(path) {
        Some(i) => i,
        None => return Ok(stats),
    };
    let key = format!("spool_offset:{identity}");

    // Stored as `offset,fingerprint`.
    let stored = crate::db::get_state(conn, &key);
    let (mut offset, stored_fp) = match stored.as_deref() {
        Some(v) => {
            let mut parts = v.split(',');
            let off = parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let fp = parts.next().and_then(|s| s.parse::<u64>().ok());
            (off, fp)
        }
        None => (0, None),
    };

    let len = std::fs::metadata(path)?.len();
    if len < offset {
        tracing::warn!(path = %path.display(), "spool shrank; restarting from 0");
        offset = 0;
    } else if stored_fp.is_some_and(|fp| fp != head_fingerprint(path, offset)) {
        tracing::warn!(path = %path.display(), "spool was rewritten in place; restarting from 0");
        offset = 0;
    }
    if len == offset {
        return Ok(stats);
    }

    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO events (session_id, event_type, ts, cwd, transcript_path,
                                 permission_mode, notification_kind, tool_name, ppid,
                                 term_program, term_session_id, raw)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )?;

        let mut buf = Vec::new();
        let mut consumed = offset;
        loop {
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            // A trailing line with no newline is a partial write still in
            // flight. Leave it for the next drain.
            if buf.last() != Some(&b'\n') {
                break;
            }
            consumed += n as u64;
            stats.lines_read += 1;

            let line = String::from_utf8_lossy(&buf);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let ev: SpoolEvent = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(e) => {
                    stats.lines_skipped += 1;
                    tracing::debug!(error = %e, "skipping unparseable spool line");
                    continue;
                }
            };
            if ev.session_id.is_empty() || ev.event.is_empty() {
                stats.lines_skipped += 1;
                continue;
            }
            stmt.execute(rusqlite::params![
                ev.session_id,
                ev.event,
                ev.ts,
                ev.cwd,
                ev.transcript_path,
                ev.permission_mode,
                ev.notification_kind,
                ev.tool_name,
                ev.ppid,
                ev.term_program,
                ev.term_session_id,
                line,
            ])?;
            stats.events_inserted += 1;
        }
        let fp = head_fingerprint(path, consumed);
        crate::db::set_state(&tx, &key, &format!("{consumed},{fp}"))?;
    }
    tx.commit()?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(session: &str, kind: &str, ts: &str) -> String {
        serde_json::to_string(&SpoolEvent {
            v: SPOOL_VERSION,
            ts: ts.into(),
            event: kind.into(),
            session_id: session.into(),
            cwd: Some("/p".into()),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn drains_and_never_re_ingests() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("events.jsonl");
        let conn = crate::db::open_memory().unwrap();

        append_line(
            &spool,
            &ev("s1", "SessionStart", "2026-07-24T10:00:00.000Z"),
        )
        .unwrap();
        append_line(&spool, &ev("s1", "Stop", "2026-07-24T10:05:00.000Z")).unwrap();

        let a = drain(&conn, &spool).unwrap();
        assert_eq!(a.events_inserted, 2);

        // Second drain with no new lines inserts nothing.
        let b = drain(&conn, &spool).unwrap();
        assert_eq!(b.events_inserted, 0);

        append_line(&spool, &ev("s1", "SessionEnd", "2026-07-24T10:06:00.000Z")).unwrap();
        let c = drain(&conn, &spool).unwrap();
        assert_eq!(c.events_inserted, 1);

        let total: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3);
    }

    #[test]
    fn rotation_loses_no_events_and_re_ingests_none() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("events.jsonl");
        let conn = crate::db::open_memory().unwrap();

        for i in 0..5 {
            append_line(
                &spool,
                &ev("s1", "PostToolUse", &format!("2026-07-24T10:0{i}:00.000Z")),
            )
            .unwrap();
        }
        assert_eq!(drain(&conn, &spool).unwrap().events_inserted, 5);

        // Force a rotation, then write more to the fresh primary.
        assert!(rotate_if_needed(&spool, 1).unwrap());
        assert!(tmp.path().join("events.1.jsonl").exists());
        for i in 5..8 {
            append_line(
                &spool,
                &ev("s1", "PostToolUse", &format!("2026-07-24T10:0{i}:00.000Z")),
            )
            .unwrap();
        }

        let after = drain(&conn, &spool).unwrap();
        assert_eq!(after.events_inserted, 3, "only the new lines");

        let total: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 8, "nothing lost, nothing duplicated");
    }

    #[test]
    fn partial_trailing_line_waits_for_the_next_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("events.jsonl");
        let conn = crate::db::open_memory().unwrap();

        append_line(&spool, &ev("s1", "Stop", "2026-07-24T10:00:00.000Z")).unwrap();
        // A line still being written: no trailing newline yet.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&spool)
            .unwrap();
        f.write_all(br#"{"v":1,"ts":"2026-07-24T10:01:00.000Z","event":"Stop","session"#)
            .unwrap();
        drop(f);

        assert_eq!(drain(&conn, &spool).unwrap().events_inserted, 1);

        // Now the writer finishes the line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&spool)
            .unwrap();
        f.write_all(b"_id\":\"s1\"}\n").unwrap();
        drop(f);

        assert_eq!(drain(&conn, &spool).unwrap().events_inserted, 1);
    }

    #[test]
    fn unknown_event_types_are_stored_not_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("events.jsonl");
        let conn = crate::db::open_memory().unwrap();

        append_line(
            &spool,
            r#"{"v":1,"ts":"2026-07-24T10:00:00.000Z","event":"SomeFutureEvent","session_id":"s1","brand_new_field":42}"#,
        )
        .unwrap();
        assert_eq!(drain(&conn, &spool).unwrap().events_inserted, 1);

        let kind: String = conn
            .query_row("SELECT event_type FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "SomeFutureEvent");
        let raw: String = conn
            .query_row("SELECT raw FROM events", [], |r| r.get(0))
            .unwrap();
        assert!(raw.contains("brand_new_field"), "raw passthrough preserved");
    }

    #[test]
    fn malformed_lines_are_skipped_without_failing_the_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("events.jsonl");
        let conn = crate::db::open_memory().unwrap();

        append_line(&spool, "not json").unwrap();
        append_line(&spool, r#"{"v":1,"event":"Stop"}"#).unwrap(); // no session_id
        append_line(&spool, &ev("s1", "Stop", "2026-07-24T10:00:00.000Z")).unwrap();

        let s = drain(&conn, &spool).unwrap();
        assert_eq!(s.events_inserted, 1);
        assert_eq!(s.lines_skipped, 2);
    }

    #[test]
    fn truncated_spool_restarts_from_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("events.jsonl");
        let conn = crate::db::open_memory().unwrap();

        append_line(&spool, &ev("s1", "Stop", "2026-07-24T10:00:00.000Z")).unwrap();
        drain(&conn, &spool).unwrap();

        std::fs::write(&spool, "").unwrap();
        append_line(&spool, &ev("s2", "Stop", "2026-07-24T11:00:00.000Z")).unwrap();
        assert_eq!(drain(&conn, &spool).unwrap().events_inserted, 1);
    }

    #[test]
    fn lines_are_kept_under_pipe_buf() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("events.jsonl");
        let huge = "x".repeat(MAX_LINE_BYTES * 2);
        append_line(&spool, &huge).unwrap();
        let len = std::fs::metadata(&spool).unwrap().len();
        assert!(len as usize <= MAX_LINE_BYTES, "got {len}");
    }

    #[test]
    fn concurrent_appends_do_not_interleave() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = std::sync::Arc::new(tmp.path().join("events.jsonl"));
        let mut handles = Vec::new();
        // 15 concurrent sessions is the user's actual load.
        for w in 0..15 {
            let spool = spool.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..40 {
                    let line = ev(&format!("s{w}"), "PostToolUse", "2026-07-24T10:00:00.000Z");
                    append_line(&spool, &line).unwrap();
                    let _ = i;
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let text = std::fs::read_to_string(&*spool).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 15 * 40);
        for line in lines {
            serde_json::from_str::<SpoolEvent>(line)
                .unwrap_or_else(|e| panic!("interleaved or truncated line: {e}\n{line}"));
        }
    }
}
