//! The weekly work report.
//!
//! A human and a chat model both consume this, so the output contract matters
//! more than the internals. Four signals per session are what make the
//! downstream ticket conversation possible without follow-up questions:
//!
//!   1. the title  2. the first prompt, verbatim
//!   3. commits with subjects  4. worktree clean + open task count
//!
//! Together they distinguish *shipped* from *in flight*, which is the
//! difference between closing a ticket and commenting on one.
//!
//! The report states facts. It does not recommend transitions, carry a
//! "status: done" field, or score completion — the reader decides. And it
//! **never includes transcript excerpts**: a week of raw transcript would
//! swamp any context window, which is the entire reason this report exists.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::model::*;
use crate::paths::{abbreviate_home, project_name};
use crate::transcript::{one_line, truncate_words};

/// File lists are truncated at this many entries.
const MAX_FILES: usize = 5;
/// `asked:` is truncated at roughly this many characters, on a word boundary.
const MAX_ASKED: usize = 200;

#[derive(Debug, Clone)]
pub struct ReportOptions {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    /// Substring match against the project path.
    pub project: Option<String>,
    /// Include sessions with no commits and no file edits.
    pub include_empty: bool,
    pub include_ended: bool,
}

impl ReportOptions {
    pub fn since(since: DateTime<Utc>) -> Self {
        Self {
            since,
            until: Utc::now(),
            project: None,
            include_empty: false,
            include_ended: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub generated_at: DateTime<Utc>,
    pub projects: Vec<ProjectReport>,
    pub total_sessions: usize,
    pub total_commits: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectReport {
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
    /// `None` when the project is not a git repo or git was unreachable.
    pub worktree_dirty: Option<bool>,
    pub open_todos: i64,
    pub sessions: Vec<SessionReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReport {
    pub session_id: String,
    pub title: String,
    pub asked: Option<String>,
    pub state: SessionState,
    pub stale: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub commits: Vec<ReportCommit>,
    pub files: Vec<String>,
    pub files_total: usize,
    pub open_todos: i64,
    pub tickets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCommit {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub ts: DateTime<Utc>,
    pub confidence: Confidence,
}

/// Build a report from already-ingested views.
pub fn build(views: &[SessionView], cfg: &Config, opts: &ReportOptions) -> Report {
    let mut by_project: Vec<ProjectReport> = Vec::new();

    for view in views {
        let s = &view.session;

        if !opts.include_ended && view.state == SessionState::Ended {
            continue;
        }
        // A session is in range if it was active at any point in the window.
        let last = s
            .last_event_at
            .unwrap_or_else(|| s.started_at.unwrap_or(opts.since));
        let first = s.started_at.unwrap_or(last);
        if last < opts.since || first > opts.until {
            continue;
        }

        // A session is reported once per repo it actually worked in. Grouping
        // by cwd instead would file a week of work across many repos under
        // whichever directory `claude` happened to be launched from.
        let project_paths: Vec<String> = if view.projects.is_empty() {
            vec![s.project_path.clone()]
        } else {
            view.projects
                .iter()
                .map(|p| p.project_path.clone())
                .collect()
        };

        for project_path in project_paths {
            if let Some(filter) = &opts.project {
                if !project_path.contains(filter.as_str()) {
                    continue;
                }
            }

            let commits: Vec<ReportCommit> = view
                .commits
                .iter()
                .filter(|c| c.commit.project_path == project_path)
                .filter(|c| c.commit.ts >= opts.since && c.commit.ts <= opts.until)
                .map(|c| ReportCommit {
                    short_sha: short_sha(&c.commit.sha),
                    sha: c.commit.sha.clone(),
                    subject: scrub(cfg, &c.commit.subject),
                    ts: c.commit.ts,
                    confidence: c.confidence,
                })
                .collect();

            // Component-wise, not a string prefix: `/a/bc` does not live in
            // `/a/b`, and on Windows the two sides disagree about separators.
            let project_files: Vec<&FileEdit> = view
                .files
                .iter()
                .filter(|f| crate::paths::is_inside(&f.path, &project_path))
                .collect();
            // Files only land outside every repo when the fallback cwd row is
            // in play; then the session's whole file list is the right answer.
            let project_files: Vec<&FileEdit> =
                if project_files.is_empty() && view.projects.len() <= 1 {
                    view.files.iter().collect()
                } else {
                    project_files
                };

            if !opts.include_empty && commits.is_empty() && project_files.is_empty() {
                continue;
            }

            let files: Vec<String> = project_files
                .iter()
                .take(MAX_FILES)
                .map(|f| relative_to(&f.path, &project_path))
                .collect();

            // Redact before truncating, so a key cut in half by the word limit
            // still cannot survive as a recognisable fragment.
            let asked = s.first_prompt.as_deref().map(|p| {
                let cleaned = if cfg.redact_secrets {
                    crate::redact::redact(p)
                } else {
                    p.to_string()
                };
                truncate_words(&one_line(&cleaned), MAX_ASKED)
            });

            let session = SessionReport {
                session_id: s.session_id.clone(),
                title: scrub(cfg, &s.display_title()),
                asked,
                state: view.state,
                stale: view.stale,
                started_at: s.started_at,
                last_activity_at: s.last_event_at,
                commits,
                files,
                files_total: project_files.len(),
                open_todos: view.open_todos,
                tickets: s.ticket_keys.clone(),
            };

            match by_project.iter_mut().find(|p| p.path == project_path) {
                Some(p) => {
                    p.open_todos += view.open_todos;
                    p.sessions.push(session);
                }
                None => {
                    let info = crate::git::repo_info(
                        std::path::Path::new(&project_path),
                        cfg.git_timeout_secs,
                        cfg.git_cache_ttl_secs,
                    );
                    by_project.push(ProjectReport {
                        name: project_name(&project_path),
                        path: project_path.clone(),
                        branch: info.branch,
                        worktree_dirty: info.dirty,
                        open_todos: view.open_todos,
                        sessions: vec![session],
                    })
                }
            }
        }
    }

    // Busiest project first; within a project, oldest session first so the
    // report reads chronologically.
    by_project.sort_by(|a, b| {
        b.sessions
            .len()
            .cmp(&a.sessions.len())
            .then(a.name.cmp(&b.name))
    });
    for p in &mut by_project {
        p.sessions.sort_by_key(|s| s.started_at);
    }

    let total_sessions = by_project.iter().map(|p| p.sessions.len()).sum();
    // Unique commits, not the sum of attributions. Long-lived overlapping
    // sessions legitimately claim the same commit, so summing would report far
    // more work than actually shipped.
    let total_commits = by_project
        .iter()
        .flat_map(|p| &p.sessions)
        .flat_map(|s| &s.commits)
        .map(|c| c.sha.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    Report {
        since: opts.since,
        until: opts.until,
        generated_at: Utc::now(),
        projects: by_project,
        total_sessions,
        total_commits,
    }
}

/// Markdown, because the destination is a chat window.
///
/// Terseness is a feature: aim for roughly one screen per project.
pub fn render_markdown(r: &Report) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "# Work report · {} → {}\n",
        local_date(&r.since),
        local_date(&r.until)
    ));
    out.push_str(&format!(
        "{} · {} · {}\n",
        plural(r.projects.len(), "project"),
        plural(r.total_sessions, "session"),
        plural(r.total_commits, "commit")
    ));

    if r.projects.is_empty() {
        out.push_str("\nNo sessions in range.\n");
        return out;
    }

    for p in &r.projects {
        out.push_str(&format!("\n## {}\n", p.name));

        let mut meta = vec![format!("`{}`", abbreviate_home(&p.path))];
        if let Some(b) = &p.branch {
            meta.push(format!("branch `{b}`"));
        }
        meta.push(match p.worktree_dirty {
            Some(true) => "worktree dirty".to_string(),
            Some(false) => "worktree clean".to_string(),
            None => "not a git repo".to_string(),
        });
        meta.push(plural(p.open_todos as usize, "pending todo"));
        out.push_str(&format!("{}\n", meta.join(" · ")));

        for s in &p.sessions {
            out.push_str(&format!("\n### {}\n", s.title));
            if let Some(asked) = &s.asked {
                out.push_str(&format!("asked: \"{asked}\"\n"));
            }

            let mut line = vec![format!(
                "{} → {}",
                local_datetime(&s.started_at),
                local_datetime(&s.last_activity_at)
            )];
            line.push(plural(s.commits.len(), "commit"));
            line.push(plural(s.files_total, "file"));
            if s.stale {
                line.push("stale".to_string());
            }
            out.push_str(&format!("{}\n", line.join(" · ")));

            for c in &s.commits {
                let mark = match c.confidence {
                    Confidence::Exact => String::new(),
                    // Rendered so the human can arbitrate; we do not guess.
                    Confidence::Window => " _(window)_".to_string(),
                };
                out.push_str(&format!("- `{}` {}{}\n", c.short_sha, c.subject, mark));
            }

            if s.commits.is_empty() {
                let mut status = vec!["no commits".to_string()];
                if s.open_todos > 0 {
                    status.push(plural(s.open_todos as usize, "pending todo"));
                }
                out.push_str(&format!("{}\n", status.join(" · ")));
            }

            if !s.files.is_empty() {
                let extra = s.files_total.saturating_sub(s.files.len());
                let suffix = if extra > 0 {
                    format!(" (+{extra} more)")
                } else {
                    String::new()
                };
                out.push_str(&format!("files: {}{}\n", s.files.join(", "), suffix));
            }

            if !s.tickets.is_empty() {
                out.push_str(&format!("tickets: {}\n", s.tickets.join(", ")));
            }
        }
    }
    out
}

/// Mask credentials, when the user has not turned that off.
fn scrub(cfg: &Config, text: &str) -> String {
    if cfg.redact_secrets {
        crate::redact::redact(text)
    } else {
        text.to_string()
    }
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn relative_to(path: &str, project: &str) -> String {
    crate::paths::relative_within(path, project).unwrap_or_else(|| abbreviate_home(path))
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn local_date(ts: &DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%Y-%m-%d").to_string()
}

fn local_datetime(ts: &Option<DateTime<Utc>>) -> String {
    match ts {
        Some(t) => t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string(),
        None => "?".to_string(),
    }
}

/// Parse `--until`: the same vocabulary as `--since`, plus `now`.
///
/// A bare date means the **end** of that day, so `--since=2026-07-01
/// --until=2026-07-31` covers all of July rather than stopping at midnight on
/// the 31st and silently dropping a day's work.
pub fn parse_until(input: &str) -> Result<DateTime<Utc>> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() || s == "now" {
        return Ok(Utc::now());
    }
    if let Ok(date) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
        return end_of_local_day(date);
    }
    // `today` as an end bound means the end of today, not this morning.
    match s.as_str() {
        "today" => end_of_local_day(Local::now().date_naive()),
        "yesterday" => end_of_local_day((Local::now() - Duration::days(1)).date_naive()),
        _ => parse_since(&s),
    }
}

fn end_of_local_day(d: NaiveDate) -> Result<DateTime<Utc>> {
    let naive = d
        .and_hms_opt(23, 59, 59)
        .ok_or_else(|| anyhow!("invalid date"))?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|d| d.with_timezone(&Utc))
        .ok_or_else(|| anyhow!("ambiguous local time for {d}"))
}

/// Parse `--since`: `monday`, `today`, `yesterday`, `week`, `Nd`, or an ISO date.
///
/// All relative forms resolve against local time, because "monday" means the
/// user's Monday.
pub fn parse_since(input: &str) -> Result<DateTime<Utc>> {
    let s = input.trim().to_ascii_lowercase();
    let now = Local::now();

    let start_of = |d: NaiveDate| -> Result<DateTime<Utc>> {
        let naive = d
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow!("invalid date"))?;
        Local
            .from_local_datetime(&naive)
            .single()
            .map(|d| d.with_timezone(&Utc))
            .ok_or_else(|| anyhow!("ambiguous local time for {d}"))
    };

    match s.as_str() {
        "monday" => {
            let back = now.weekday().num_days_from_monday() as i64;
            start_of((now - Duration::days(back)).date_naive())
        }
        "today" => start_of(now.date_naive()),
        "yesterday" => start_of((now - Duration::days(1)).date_naive()),
        "week" => Ok((now - Duration::days(7)).with_timezone(&Utc)),
        _ => {
            if let Some(days) = s.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
                return Ok((now - Duration::days(days)).with_timezone(&Utc));
            }
            if let Ok(d) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                return start_of(d);
            }
            Err(anyhow!(
                "could not parse --since '{input}'; try monday, today, yesterday, week, 7d, or 2026-07-01"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, project: &str, title: &str) -> SessionView {
        SessionView {
            session: Session {
                session_id: id.into(),
                project_path: project.into(),
                project_slug: "-p".into(),
                summary: Some(title.into()),
                first_prompt: Some("update the pricing section and swap the hero copy".into()),
                started_at: parse_ts("2026-07-21T09:14:00.000Z"),
                last_event_at: parse_ts("2026-07-22T16:02:00.000Z"),
                ..Default::default()
            },
            state: SessionState::Idle,
            stale: false,
            action_kind: None,
            liveness: Liveness::Unknown,
            worktree_dirty: Some(false),
            open_todos: 0,
            projects: vec![ProjectEdits {
                project_path: project.into(),
                edits: 3,
            }],
            files: vec![FileEdit {
                path: format!("{project}/src/Pricing.tsx"),
                edits: 3,
            }],
            commits: vec![AttributedCommit {
                commit: Commit {
                    sha: "a3f21e9ffff".into(),
                    project_path: project.into(),
                    ts: parse_ts("2026-07-21T10:00:00.000Z").unwrap(),
                    subject: "fix pricing tier copy".into(),
                    author_email: None,
                    branch: Some("main".into()),
                },
                confidence: Confidence::Exact,
            }],
        }
    }

    fn opts() -> ReportOptions {
        ReportOptions {
            since: parse_ts("2026-07-20T00:00:00.000Z").unwrap(),
            until: parse_ts("2026-07-25T00:00:00.000Z").unwrap(),
            project: None,
            include_empty: false,
            include_ended: true,
        }
    }

    #[test]
    fn renders_the_documented_shape() {
        let views = vec![view(
            "s1",
            "/dev/storefront",
            "Add checkout flow and wire up payments",
        )];
        let r = build(&views, &Config::default(), &opts());
        let md = render_markdown(&r);

        assert!(md.starts_with("# Work report · "));
        assert!(md.contains("1 project · 1 session · 1 commit"));
        assert!(md.contains("## storefront"));
        assert!(md.contains("### Add checkout flow and wire up payments"));
        assert!(md.contains("asked: \"update the pricing section"));
        assert!(md.contains("- `a3f21e9` fix pricing tier copy"));
        assert!(md.contains("files: src/Pricing.tsx"));
        // Never leak transcript content beyond the first prompt.
        assert!(!md.contains("tool_use"));
    }

    #[test]
    fn empty_sessions_are_omitted_unless_requested() {
        let mut v = view("s1", "/p", "Nothing happened");
        v.commits.clear();
        v.files.clear();

        let r = build(&[v.clone()], &Config::default(), &opts());
        assert_eq!(r.total_sessions, 0);
        assert!(render_markdown(&r).contains("No sessions in range."));

        let mut o = opts();
        o.include_empty = true;
        let r = build(&[v], &Config::default(), &o);
        assert_eq!(r.total_sessions, 1);
        assert!(render_markdown(&r).contains("no commits"));
    }

    #[test]
    fn sessions_outside_the_window_are_excluded() {
        let mut v = view("s1", "/p", "Old work");
        v.session.started_at = parse_ts("2026-01-01T00:00:00.000Z");
        v.session.last_event_at = parse_ts("2026-01-02T00:00:00.000Z");
        let r = build(&[v], &Config::default(), &opts());
        assert_eq!(r.total_sessions, 0);
    }

    #[test]
    fn commits_outside_the_window_are_dropped_from_a_session_in_range() {
        let mut v = view("s1", "/p", "Mixed");
        v.commits[0].commit.ts = parse_ts("2026-01-01T00:00:00.000Z").unwrap();
        let r = build(&[v], &Config::default(), &opts());
        // Still reported (it has file edits), but with no commits.
        assert_eq!(r.total_commits, 0);
        assert!(render_markdown(&r).contains("no commits"));
    }

    #[test]
    fn ended_sessions_can_be_excluded() {
        let mut v = view("s1", "/p", "Finished");
        v.state = SessionState::Ended;
        let mut o = opts();
        o.include_ended = false;
        assert_eq!(
            build(&[v.clone()], &Config::default(), &o).total_sessions,
            0
        );
        o.include_ended = true;
        assert_eq!(build(&[v], &Config::default(), &o).total_sessions, 1);
    }

    #[test]
    fn a_session_spanning_repos_appears_under_each_with_only_its_own_work() {
        // The case that motivated splitting by repo: `claude` launched from
        // $HOME, editing two unrelated repos in one session. Grouping by cwd
        // would file all of it under a non-repo bucket with no commits.
        let mut v = view("s1", "/home/user", "Cross-repo session");
        v.session.project_path = "/home/user".into();
        v.projects = vec![
            ProjectEdits {
                project_path: "/repos/alpha".into(),
                edits: 4,
            },
            ProjectEdits {
                project_path: "/repos/beta".into(),
                edits: 1,
            },
        ];
        v.files = vec![
            FileEdit {
                path: "/repos/alpha/a.ts".into(),
                edits: 4,
            },
            FileEdit {
                path: "/repos/beta/b.ts".into(),
                edits: 1,
            },
        ];
        v.commits = vec![AttributedCommit {
            commit: Commit {
                sha: "deadbeef123".into(),
                project_path: "/repos/alpha".into(),
                ts: parse_ts("2026-07-21T10:00:00.000Z").unwrap(),
                subject: "alpha only".into(),
                author_email: None,
                branch: None,
            },
            confidence: Confidence::Exact,
        }];

        let r = build(&[v], &Config::default(), &opts());
        assert_eq!(r.projects.len(), 2);

        let alpha = r.projects.iter().find(|p| p.name == "alpha").unwrap();
        let beta = r.projects.iter().find(|p| p.name == "beta").unwrap();

        assert_eq!(alpha.sessions[0].files, vec!["a.ts"]);
        assert_eq!(alpha.sessions[0].commits.len(), 1);
        assert_eq!(beta.sessions[0].files, vec!["b.ts"]);
        assert_eq!(
            beta.sessions[0].commits.len(),
            0,
            "alpha's commit must not leak into beta"
        );
    }

    #[test]
    fn credentials_never_reach_the_report() {
        // The report exists to be pasted into a chat, so anything reproduced
        // verbatim is a potential disclosure.
        let mut v = view(
            "s1",
            "/p",
            "Deploy with ghp_AbCdEfGhIjKlMnOpQrStUvWxYz012345",
        );
        v.session.first_prompt = Some("use sk-abcdefghijklmnopqrstuvwx1234 to call the api".into());
        v.commits[0].commit.subject = "wire in AKIAIOSFODNN7EXAMPLE".into();

        let md = render_markdown(&build(&[v.clone()], &Config::default(), &opts()));
        assert!(!md.contains("sk-abcdefghijklmnopqrstuvwx1234"), "{md}");
        assert!(!md.contains("ghp_AbCdEfGhIjKlMnOpQrStUvWxYz012345"), "{md}");
        assert!(!md.contains("AKIAIOSFODNN7EXAMPLE"), "{md}");
        assert!(md.contains(crate::redact::MASK), "{md}");

        // Opting out is honoured, because it is the user's data.
        let off = Config {
            redact_secrets: false,
            ..Default::default()
        };
        let md = render_markdown(&build(&[v], &off, &opts()));
        assert!(md.contains("sk-abcdefghijklmnopqrstuvwx1234"));
    }

    #[test]
    fn ordinary_reports_are_untouched_by_redaction() {
        let r = build(
            &[view("s1", "/p", "Normal work")],
            &Config::default(),
            &opts(),
        );
        let md = render_markdown(&r);
        assert!(!md.contains(crate::redact::MASK), "{md}");
        assert!(md.contains("update the pricing section"), "{md}");
    }

    #[test]
    fn the_header_counts_unique_commits_not_attributions() {
        // Two overlapping sessions both legitimately claim one commit. The
        // report must not then say two commits shipped.
        let a = view("s1", "/p", "Session A");
        let b = view("s2", "/p", "Session B");
        let r = build(&[a, b], &Config::default(), &opts());
        assert_eq!(r.total_sessions, 2);
        assert_eq!(r.total_commits, 1);
        assert!(render_markdown(&r).contains("1 commit"));
    }

    #[test]
    fn project_filter_is_a_substring_match() {
        let views = vec![view("s1", "/dev/alpha", "A"), view("s2", "/dev/beta", "B")];
        let mut o = opts();
        o.project = Some("beta".into());
        let r = build(&views, &Config::default(), &o);
        assert_eq!(r.projects.len(), 1);
        assert_eq!(r.projects[0].name, "beta");
    }

    #[test]
    fn file_lists_truncate_with_a_count() {
        let mut v = view("s1", "/p", "Many files");
        v.files = (0..9)
            .map(|i| FileEdit {
                path: format!("/p/file{i}.ts"),
                edits: 1,
            })
            .collect();
        let md = render_markdown(&build(&[v], &Config::default(), &opts()));
        assert!(md.contains("(+4 more)"), "{md}");
    }

    #[test]
    fn window_confidence_is_visible_to_the_reader() {
        let mut v = view("s1", "/p", "Shared");
        v.commits[0].confidence = Confidence::Window;
        let md = render_markdown(&build(&[v], &Config::default(), &opts()));
        assert!(md.contains("_(window)_"));
    }

    #[test]
    fn asked_is_truncated_on_a_word_boundary() {
        let mut v = view("s1", "/p", "Long ask");
        v.session.first_prompt = Some("word ".repeat(200));
        let md = render_markdown(&build(&[v], &Config::default(), &opts()));
        let asked = md.lines().find(|l| l.starts_with("asked:")).unwrap();
        assert!(asked.contains('…'));
        assert!(asked.chars().count() < MAX_ASKED + 40);
    }

    #[test]
    fn multi_line_prompts_collapse_to_one_line() {
        let mut v = view("s1", "/p", "Multi");
        v.session.first_prompt = Some("first line\n\nsecond line".into());
        let md = render_markdown(&build(&[v], &Config::default(), &opts()));
        assert!(md.contains("asked: \"first line second line\""));
    }

    #[test]
    fn until_defaults_to_now() {
        let before = Utc::now();
        for input in ["", "now", "  "] {
            let t = parse_until(input).unwrap();
            assert!(t >= before - Duration::seconds(1));
        }
    }

    #[test]
    fn until_a_bare_date_means_the_end_of_that_day() {
        // Otherwise `--until=2026-07-31` would silently drop the 31st's work.
        let until = parse_until("2026-07-31").unwrap();
        let since = parse_since("2026-07-31").unwrap();
        assert!(until > since);
        assert_eq!((until - since).num_hours(), 23);
    }

    #[test]
    fn a_closed_historical_window_excludes_later_work() {
        let mut v = view("s1", "/p", "June work");
        v.session.started_at = parse_ts("2026-06-10T09:00:00.000Z");
        v.session.last_event_at = parse_ts("2026-06-10T17:00:00.000Z");
        v.commits[0].commit.ts = parse_ts("2026-06-10T12:00:00.000Z").unwrap();

        let june = ReportOptions {
            since: parse_ts("2026-06-01T00:00:00.000Z").unwrap(),
            until: parse_ts("2026-06-30T23:59:59.000Z").unwrap(),
            project: None,
            include_empty: false,
            include_ended: true,
        };
        assert_eq!(
            build(&[v.clone()], &Config::default(), &june).total_sessions,
            1
        );

        // The same session must not appear in a July report.
        let july = ReportOptions {
            since: parse_ts("2026-07-01T00:00:00.000Z").unwrap(),
            until: parse_ts("2026-07-31T23:59:59.000Z").unwrap(),
            ..june
        };
        assert_eq!(build(&[v], &Config::default(), &july).total_sessions, 0);
    }

    #[test]
    fn since_accepts_every_documented_form() {
        for s in ["monday", "today", "yesterday", "week", "7d", "2026-07-01"] {
            assert!(parse_since(s).is_ok(), "{s} should parse");
        }
        assert!(parse_since("next tuesday").is_err());
        assert!(parse_since("").is_err());
    }

    #[test]
    fn since_monday_is_not_in_the_future() {
        let monday = parse_since("monday").unwrap();
        assert!(monday <= Utc::now());
        assert!(Utc::now() - monday < Duration::days(7));
    }

    #[test]
    fn since_nd_goes_back_n_days() {
        let d = parse_since("7d").unwrap();
        let delta = (Utc::now() - d).num_hours();
        assert!((167..=169).contains(&delta), "got {delta}h");
    }
}
