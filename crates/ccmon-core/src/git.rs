//! Git collection by shelling out to `git`.
//!
//! We only need `log`, `status --porcelain`, and `rev-parse`. Shelling out
//! respects the user's git config, behaves identically to what they see in
//! their own terminal (includes, hooks, worktrees), and keeps a heavy C
//! dependency out of the build that every contributor would have to compile.
//!
//! Git is ground truth for what shipped. Transcripts are evidence of what was
//! attempted.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::model::{parse_ts, Commit};

/// Unit and record separators. Commit subjects can contain almost anything,
/// including newlines, so those are the only safe delimiters.
const SEP_FIELD: char = '\x1f';
const SEP_RECORD: char = '\x1e';

#[derive(Debug, Clone, Default)]
pub struct RepoInfo {
    pub is_repo: bool,
    pub branch: Option<String>,
    /// `None` means we could not tell (not a repo, or git timed out).
    pub dirty: Option<bool>,
    pub author_email: Option<String>,
}

struct CacheEntry {
    at: Instant,
    info: RepoInfo,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clear the repo caches. Tests and long-lived processes need this.
pub fn clear_cache() {
    if let Ok(mut c) = cache().lock() {
        c.clear();
    }
    if let Ok(mut c) = root_cache().lock() {
        c.clear();
    }
}

/// Repo status for a project, cached briefly so a report across 20 projects
/// does not run 60 git commands.
pub fn repo_info(dir: &Path, timeout_secs: u64, ttl_secs: u64) -> RepoInfo {
    let key = dir.to_path_buf();
    if let Ok(c) = cache().lock() {
        if let Some(entry) = c.get(&key) {
            if entry.at.elapsed() < Duration::from_secs(ttl_secs) {
                return entry.info.clone();
            }
        }
    }

    let timeout = Duration::from_secs(timeout_secs);
    let mut info = RepoInfo::default();

    let inside = run_git(dir, &["rev-parse", "--is-inside-work-tree"], timeout);
    info.is_repo = inside.as_deref().map(str::trim) == Some("true");

    if info.is_repo {
        info.branch = run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"], timeout)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        info.dirty =
            run_git(dir, &["status", "--porcelain"], timeout).map(|s| !s.trim().is_empty());
        info.author_email = run_git(dir, &["config", "user.email"], timeout)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }

    if let Ok(mut c) = cache().lock() {
        c.insert(
            key,
            CacheEntry {
                at: Instant::now(),
                info: info.clone(),
            },
        );
    }
    info
}

/// The git repo root containing `dir`, or `None` if it is not in a repo.
///
/// A session's cwd is often not its project — running `claude` from `$HOME`
/// while editing files across several repos is normal — so the repo a file
/// lives in is what actually identifies the project.
pub fn repo_root(dir: &Path, timeout_secs: u64) -> Option<PathBuf> {
    // Files get deleted and directories get moved; walk up to something that
    // still exists rather than failing outright.
    let mut probe = dir;
    while !probe.exists() {
        probe = probe.parent()?;
    }

    let key = probe.to_path_buf();
    if let Ok(c) = root_cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }

    let root = run_git(
        probe,
        &["rev-parse", "--show-toplevel"],
        Duration::from_secs(timeout_secs),
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .map(PathBuf::from);

    if let Ok(mut c) = root_cache().lock() {
        c.insert(key, root.clone());
    }
    root
}

fn root_cache() -> &'static Mutex<HashMap<PathBuf, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Commits on the current branch since `since`, optionally filtered to one author.
///
/// `git log` walks HEAD only, so `branch` on every returned commit is the
/// branch checked out at scan time. That is the same view the user has.
pub fn log_since(
    dir: &Path,
    project_path: &str,
    since: DateTime<Utc>,
    author_email: Option<&str>,
    branch: Option<&str>,
    timeout_secs: u64,
) -> Vec<Commit> {
    let since_arg = format!("--since={}", since.to_rfc3339());
    let format_arg = format!("--format=%H{SEP_FIELD}%aI{SEP_FIELD}%s{SEP_FIELD}%aE{SEP_RECORD}");
    let mut args: Vec<&str> = vec!["log", "--no-merges", &since_arg, &format_arg];
    let author_arg;
    if let Some(email) = author_email.filter(|e| !e.is_empty()) {
        author_arg = format!("--author={email}");
        args.push(&author_arg);
    }

    let out = match run_git(dir, &args, Duration::from_secs(timeout_secs)) {
        Some(o) => o,
        None => return Vec::new(),
    };

    out.split(SEP_RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches(['\n', '\r']);
            if record.trim().is_empty() {
                return None;
            }
            let mut parts = record.split(SEP_FIELD);
            let sha = parts.next()?.trim().to_string();
            let ts = parse_ts(parts.next()?.trim())?;
            let subject = parts.next()?.trim().to_string();
            let author = parts.next().map(|s| s.trim().to_string());
            if sha.is_empty() {
                return None;
            }
            Some(Commit {
                sha,
                project_path: project_path.to_string(),
                ts,
                subject,
                author_email: author.filter(|a| !a.is_empty()),
                branch: branch.map(|b| b.to_string()),
            })
        })
        .collect()
}

/// Run a git command, returning stdout on success.
///
/// Returns `None` on any failure — missing git, non-zero exit, or timeout. A
/// slow or broken repo must never hang the app, so the child is killed at the
/// deadline and stdout is drained on a worker thread so a full pipe buffer
/// cannot deadlock us.
fn run_git(dir: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    if !dir.exists() {
        return None;
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stdout = stdout;
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    tracing::warn!(dir = %dir.display(), ?args, "git timed out; killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    };

    let bytes = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    if !status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    #[test]
    fn non_repo_reports_not_a_repo() {
        clear_cache();
        let tmp = tempfile::tempdir().unwrap();
        let info = repo_info(tmp.path(), 5, 0);
        assert!(!info.is_repo);
        assert_eq!(info.dirty, None, "unknown, not clean");
    }

    #[test]
    fn missing_directory_is_not_a_repo() {
        clear_cache();
        let info = repo_info(Path::new("/nonexistent/ccmon/path"), 5, 0);
        assert!(!info.is_repo);
    }

    #[test]
    fn detects_branch_dirtiness_and_commits() {
        clear_cache();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        git(dir, &["add", "."]);
        git(
            dir,
            &[
                "commit",
                "-qm",
                "first: add a subject with, commas and 'quotes'",
            ],
        );

        let info = repo_info(dir, 5, 0);
        assert!(info.is_repo);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.dirty, Some(false));
        assert_eq!(info.author_email.as_deref(), Some("test@example.com"));

        let since = Utc::now() - chrono::Duration::days(1);
        let commits = log_since(dir, "/p", since, None, Some("main"), 5);
        assert_eq!(commits.len(), 1);
        assert_eq!(
            commits[0].subject,
            "first: add a subject with, commas and 'quotes'"
        );
        assert_eq!(commits[0].project_path, "/p");
        assert_eq!(commits[0].branch.as_deref(), Some("main"));

        std::fs::write(dir.join("b.txt"), "dirty").unwrap();
        clear_cache();
        assert_eq!(repo_info(dir, 5, 0).dirty, Some(true));
    }

    #[test]
    fn author_filter_excludes_other_authors() {
        clear_cache();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init_repo(dir);
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "mine"]);

        let since = Utc::now() - chrono::Duration::days(1);
        assert_eq!(
            log_since(dir, "/p", since, Some("test@example.com"), None, 5).len(),
            1
        );
        assert_eq!(
            log_since(dir, "/p", since, Some("someone@else.com"), None, 5).len(),
            0
        );
    }

    #[test]
    fn cache_ttl_serves_repeated_reads() {
        clear_cache();
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let a = repo_info(tmp.path(), 5, 30);
        // Second read comes from cache; must be identical.
        let b = repo_info(tmp.path(), 5, 30);
        assert_eq!(a.is_repo, b.is_repo);
        assert_eq!(a.branch, b.branch);
    }
}
