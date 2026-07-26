//! End-to-end: a synthetic Claude Code root plus a real git repo, ingested and
//! reported the same way the binary does it.
//!
//! These are the tests that would have caught the two bugs found against real
//! data: transcripts nested under `subagents/`, and a session whose cwd is not
//! its project.

use ccmon_core::rusqlite;
use std::path::Path;
use std::process::{Command, Stdio};

use ccmon_core::{config::Config, db, ingest, model::SessionState, report, store};
use chrono::{Duration, Utc};

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
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "dev@example.com"]);
    git(dir, &["config", "user.name", "Dev"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// A transcript in the current on-disk format.
fn transcript(session_id: &str, cwd: &str, title: &str, prompt: &str, edits: &[&str]) -> String {
    let now = Utc::now();
    let started = (now - Duration::minutes(30)).to_rfc3339();
    let ended = (now - Duration::minutes(1)).to_rfc3339();

    let mut lines = vec![
        format!(r#"{{"type":"mode","mode":"normal","sessionId":"{session_id}"}}"#),
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{prompt}"}},"timestamp":"{started}","cwd":"{cwd}","sessionId":"{session_id}","gitBranch":"main"}}"#
        ),
        format!(r#"{{"type":"ai-title","aiTitle":"{title}","sessionId":"{session_id}"}}"#),
    ];
    for path in edits {
        lines.push(format!(
            r#"{{"type":"assistant","timestamp":"{ended}","sessionId":"{session_id}","cwd":"{cwd}","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Edit","input":{{"file_path":"{path}"}}}}]}}}}"#
        ));
    }
    lines.join("\n")
}

struct Fixture {
    _tmp: tempfile::TempDir,
    conn: rusqlite::Connection,
    cfg: Config,
    repo: std::path::PathBuf,
}

/// Build a Claude root whose session runs from $HOME but edits a real repo,
/// with a subagent transcript nested underneath it.
fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("claude");
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repos").join("alpha");
    std::fs::create_dir_all(&home).unwrap();
    init_repo(&repo);

    let session = "11111111-2222-3333-4444-555555555555";
    let project_dir = root.join("projects").join("-home");
    std::fs::create_dir_all(&project_dir).unwrap();

    // The session's cwd is $HOME, but its edits land in the repo.
    let main_file = repo.join("main.rs");
    std::fs::write(&main_file, "fn main() {}\n").unwrap();
    std::fs::write(
        project_dir.join(format!("{session}.jsonl")),
        transcript(
            session,
            home.to_str().unwrap(),
            "Ship the alpha feature",
            "please ship the alpha feature",
            &[main_file.to_str().unwrap()],
        ),
    )
    .unwrap();

    // A subagent transcript, nested exactly the way Claude Code nests them.
    let sub_dir = project_dir.join(session).join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let helper = repo.join("helper.rs");
    std::fs::write(&helper, "pub fn helper() {}\n").unwrap();
    std::fs::write(
        sub_dir.join("agent-aaaa1111.jsonl"),
        transcript(
            "agent-aaaa1111",
            home.to_str().unwrap(),
            "subagent",
            "do the sub task",
            &[helper.to_str().unwrap()],
        ),
    )
    .unwrap();

    // A real commit inside the session's time window.
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "ALPHA-7 ship the alpha feature"]);

    // Open tasks, current layout.
    let tasks = root.join("tasks").join(session);
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(
        tasks.join("1.json"),
        r#"{"id":"1","subject":"finish the docs","status":"pending"}"#,
    )
    .unwrap();

    let cfg = Config {
        claude_roots: vec![root],
        only_configured_roots: true,
        ..Default::default()
    };
    let conn = db::open_memory().unwrap();
    ccmon_core::git::clear_cache();
    Fixture {
        _tmp: tmp,
        conn,
        cfg,
        repo,
    }
}

#[test]
fn ingests_a_session_and_attributes_its_repo_work() {
    let f = setup();
    let stats = ingest::run(&f.conn, &f.cfg).unwrap();

    assert_eq!(stats.sessions, 1, "the subagent is not its own session");
    assert_eq!(stats.transcripts_parsed, 2, "main transcript plus subagent");

    let views = store::build_views(&f.conn, &f.cfg, Utc::now()).unwrap();
    assert_eq!(views.len(), 1);
    let v = &views[0];

    assert_eq!(v.session.display_title(), "Ship the alpha feature");
    assert_eq!(
        v.session.first_prompt.as_deref(),
        Some("please ship the alpha feature"),
        "the subagent's prompt must not become the session's"
    );

    // Both the session's own edit and its subagent's are counted.
    assert_eq!(v.files.len(), 2, "got {:?}", v.files);

    // The project is the repo the work landed in, not the cwd it ran from.
    // `git rev-parse --show-toplevel` resolves symlinks, so compare canonical
    // paths — on macOS the temp dir is /var, which is a link to /private/var.
    assert_eq!(
        std::fs::canonicalize(v.primary_project()).unwrap(),
        std::fs::canonicalize(&f.repo).unwrap()
    );
    assert_ne!(v.session.project_path, v.primary_project());

    assert_eq!(v.commits.len(), 1);
    assert_eq!(
        v.commits[0].commit.subject,
        "ALPHA-7 ship the alpha feature"
    );
    assert!(v.session.ticket_keys.contains(&"ALPHA-7".to_string()));

    // An open task with a clean worktree is still work left on the table.
    assert_eq!(v.open_todos, 1);
    assert_eq!(v.state, SessionState::NeedsReview);
}

#[test]
fn report_groups_by_repo_and_lists_the_commit() {
    let f = setup();
    ingest::run(&f.conn, &f.cfg).unwrap();
    let views = store::build_views(&f.conn, &f.cfg, Utc::now()).unwrap();

    let opts = report::ReportOptions::since(Utc::now() - Duration::days(1));
    let r = report::build(&views, &f.cfg, &opts);
    let md = report::render_markdown(&r);

    assert_eq!(r.projects.len(), 1);
    assert_eq!(r.projects[0].name, "alpha");
    assert_eq!(r.total_commits, 1);

    assert!(md.contains("## alpha"), "{md}");
    assert!(md.contains("### Ship the alpha feature"), "{md}");
    assert!(
        md.contains("asked: \"please ship the alpha feature\""),
        "{md}"
    );
    assert!(md.contains("ALPHA-7 ship the alpha feature"), "{md}");
    assert!(md.contains("tickets: ALPHA-7"), "{md}");
    assert!(md.contains("branch `main`"), "{md}");
    // Files are shown relative to the repo.
    assert!(md.contains("files: "), "{md}");
    assert!(!md.contains("tool_use"), "no transcript content leaks");
}

#[test]
fn reindex_force_reproduces_identical_derived_state() {
    let f = setup();
    ingest::run(&f.conn, &f.cfg).unwrap();

    let snapshot = |conn: &rusqlite::Connection| -> Vec<String> {
        let mut out = Vec::new();
        for (sql, label) in [
            ("SELECT session_id, project_path, summary, first_prompt, tool_calls FROM sessions ORDER BY session_id", "session"),
            ("SELECT session_id, path, edits FROM session_files ORDER BY session_id, path", "file"),
            ("SELECT session_id, project_path, edits FROM session_projects ORDER BY session_id, project_path", "project"),
            ("SELECT session_id, sha, confidence FROM session_commits ORDER BY session_id, sha", "commit"),
        ] {
            let mut stmt = conn.prepare(sql).unwrap();
            let rows = stmt
                .query_map([], |r| {
                    let mut parts = vec![label.to_string()];
                    for i in 0..r.as_ref().column_count() {
                        parts.push(
                            r.get::<_, Option<String>>(i)
                                .or_else(|_| r.get::<_, Option<i64>>(i).map(|v| v.map(|v| v.to_string())))
                                .unwrap_or(None)
                                .unwrap_or_default(),
                        );
                    }
                    Ok(parts.join("|"))
                })
                .unwrap();
            out.extend(rows.map(|r| r.unwrap()));
        }
        out
    };

    let before = snapshot(&f.conn);
    assert!(!before.is_empty());

    ccmon_core::git::clear_cache();
    ingest::reindex(&f.conn, &f.cfg).unwrap();
    let after = snapshot(&f.conn);

    assert_eq!(before, after, "reindex --force must be reproducible");
}

#[test]
fn a_second_ingest_is_a_no_op() {
    let f = setup();
    ingest::run(&f.conn, &f.cfg).unwrap();
    let again = ingest::run(&f.conn, &f.cfg).unwrap();
    assert_eq!(
        again.transcripts_parsed, 0,
        "unchanged transcripts must not be re-parsed"
    );
    assert_eq!(again.sessions, 1);
}

#[test]
fn an_empty_claude_root_produces_an_empty_report_without_failing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("claude");
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let cfg = Config {
        claude_roots: vec![root],
        only_configured_roots: true,
        ..Default::default()
    };
    let conn = db::open_memory().unwrap();
    ingest::run(&conn, &cfg).unwrap();

    let views = store::build_views(&conn, &cfg, Utc::now()).unwrap();
    assert!(views.is_empty());

    let opts = report::ReportOptions::since(Utc::now() - Duration::days(7));
    let md = report::render_markdown(&report::build(&views, &cfg, &opts));
    assert!(md.contains("No sessions in range."));
}
