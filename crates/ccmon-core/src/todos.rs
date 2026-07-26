//! Per-session task lists.
//!
//! Two on-disk layouts exist and both are supported:
//!
//! - current: `<root>/tasks/<session-id>/<n>.json`, one object per task
//!   (`{"id","subject","description","activeForm","status","blocks","blockedBy"}`)
//! - legacy:  `<root>/todos/<session-id>-agent-<agent-id>.json`, an array of
//!   `{"content","status","activeForm"}`
//!
//! Open tasks are half of what separates NEEDS_REVIEW from IDLE, so a missing
//! directory means "no information", never "nothing pending".

use std::path::Path;

use serde::Deserialize;

use crate::model::{Todo, TodoStatus};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawTask {
    /// Current format.
    subject: Option<String>,
    /// Legacy format.
    content: Option<String>,
    #[serde(rename = "activeForm")]
    active_form: Option<String>,
    status: Option<String>,
}

impl RawTask {
    fn into_todo(self) -> Option<Todo> {
        let content = self
            .subject
            .or(self.content)
            .or(self.active_form)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;
        Some(Todo {
            content,
            status: TodoStatus::parse(self.status.as_deref().unwrap_or("pending")),
        })
    }
}

/// Read tasks for every session under a Claude root.
///
/// Returns `(session_id, todos)` pairs.
pub fn scan_root(root: &Path) -> Vec<(String, Vec<Todo>)> {
    let mut out = scan_tasks_dir(&root.join("tasks"));
    out.extend(scan_legacy_todos_dir(&root.join("todos")));
    out
}

/// Current layout: one directory per session, one JSON file per task.
fn scan_tasks_dir(dir: &Path) -> Vec<(String, Vec<Todo>)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let session_id = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let mut todos = Vec::new();
        let files = match std::fs::read_dir(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        // Numeric filenames are task ids; sort so report output is stable.
        let mut task_files: Vec<_> = files
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        task_files.sort_by_key(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(i64::MAX)
        });

        for file in task_files {
            let text = match std::fs::read_to_string(&file) {
                Ok(t) => t,
                Err(_) => continue,
            };
            match serde_json::from_str::<RawTask>(&text) {
                Ok(t) => todos.extend(t.into_todo()),
                Err(e) => {
                    tracing::debug!(path = %file.display(), error = %e, "skipping task file");
                }
            }
        }

        if !todos.is_empty() {
            out.push((session_id, todos));
        }
    }
    out
}

/// Legacy layout: `<session-id>-agent-<agent-id>.json` holding an array.
fn scan_legacy_todos_dir(dir: &Path) -> Vec<(String, Vec<Todo>)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut out: Vec<(String, Vec<Todo>)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let session_id = stem.split("-agent-").next().unwrap_or(stem).to_string();
        if session_id.is_empty() {
            continue;
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let raw: Vec<RawTask> = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "skipping todo file");
                continue;
            }
        };
        let todos: Vec<Todo> = raw.into_iter().filter_map(RawTask::into_todo).collect();
        if todos.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(s, _)| *s == session_id) {
            Some((_, existing)) => existing.extend(todos),
            None => out.push((session_id, todos)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_current_tasks_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sess = root.join("tasks").join("sess-1");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("1.json"),
            r#"{"id":"1","subject":"Phase 1","activeForm":"Doing phase 1","status":"completed","blocks":[],"blockedBy":[]}"#,
        )
        .unwrap();
        std::fs::write(
            sess.join("2.json"),
            r#"{"id":"2","subject":"Phase 2","status":"pending"}"#,
        )
        .unwrap();
        std::fs::write(sess.join(".lock"), "").unwrap();
        std::fs::write(sess.join(".highwatermark"), "8").unwrap();

        let found = scan_root(root);
        assert_eq!(found.len(), 1);
        let (id, todos) = &found[0];
        assert_eq!(id, "sess-1");
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].content, "Phase 1");
        assert_eq!(todos[0].status, TodoStatus::Completed);
        assert_eq!(todos[1].status, TodoStatus::Pending);
        assert_eq!(todos.iter().filter(|t| t.status.is_open()).count(), 1);
    }

    #[test]
    fn reads_legacy_todos_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let todos_dir = root.join("todos");
        std::fs::create_dir_all(&todos_dir).unwrap();
        std::fs::write(
            todos_dir.join("sess-9-agent-sess-9.json"),
            r#"[{"content":"do a thing","status":"in_progress","activeForm":"Doing"}]"#,
        )
        .unwrap();

        let found = scan_root(root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "sess-9");
        assert_eq!(found[0].1[0].content, "do a thing");
        assert_eq!(found[0].1[0].status, TodoStatus::InProgress);
        assert!(found[0].1[0].status.is_open());
    }

    #[test]
    fn missing_dirs_are_not_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan_root(tmp.path()).is_empty());
    }

    #[test]
    fn malformed_task_files_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let sess = tmp.path().join("tasks").join("s");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(sess.join("1.json"), "{broken").unwrap();
        std::fs::write(
            sess.join("2.json"),
            r#"{"subject":"fine","status":"pending"}"#,
        )
        .unwrap();
        let found = scan_root(tmp.path());
        assert_eq!(found[0].1.len(), 1);
        assert_eq!(found[0].1[0].content, "fine");
    }

    #[test]
    fn unknown_status_defaults_to_pending() {
        let t: RawTask =
            serde_json::from_str(r#"{"subject":"x","status":"weird-new-status"}"#).unwrap();
        assert_eq!(t.into_todo().unwrap().status, TodoStatus::Pending);
    }
}
