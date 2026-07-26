//! `ccmon ls` — live triage in the terminal.
//!
//! The title column is deliberately first and widest: it is the same string
//! Claude Code writes into the terminal window title, so scanning this list
//! against the window list is how the user finds the session that wants them.

use anyhow::Result;
use ccmon_core::{
    model::{SessionState, SessionView},
    paths::project_name,
    state::in_stale_group,
    transcript::truncate_words,
};
use chrono::Utc;

use crate::Format;

pub fn render(
    views: &[SessionView],
    state_filter: Option<&str>,
    all: bool,
    project: Option<&str>,
    format: Format,
) -> Result<()> {
    let wanted = state_filter
        .map(|s| SessionState::parse(s).ok_or_else(|| anyhow::anyhow!("unknown state '{s}'")))
        .transpose()?;

    let rows: Vec<&SessionView> = views
        .iter()
        .filter(|v| wanted.map_or(true, |w| v.state == w))
        .filter(|v| {
            all || wanted.is_some() || !matches!(v.state, SessionState::Ended | SessionState::Dead)
        })
        .filter(|v| project.map_or(true, |p| v.session.project_path.contains(p)))
        .collect();

    if format == Format::Json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        println!("No sessions match. Try --all, or run `ccmon doctor` if you expected some.");
        return Ok(());
    }

    let now = Utc::now();
    let mut current_state: Option<SessionState> = None;

    for v in &rows {
        if current_state != Some(v.state) {
            let count = rows.iter().filter(|r| r.state == v.state).count();
            println!("\n{}  ({count})", v.state.as_str());
            current_state = Some(v.state);
        }

        let s = &v.session;
        let title = truncate_words(&s.display_title(), 52);
        let age = match s.last_event_at {
            Some(t) => relative(now.signed_duration_since(t)),
            None => "?".to_string(),
        };

        let mut tags: Vec<String> = Vec::new();
        if let Some(kind) = v.action_kind {
            tags.push(kind.as_str().to_string());
        }
        if in_stale_group(v.state, v.stale) {
            tags.push("stale".to_string());
        }
        if v.worktree_dirty == Some(true) {
            tags.push("dirty".to_string());
        }
        if v.open_todos > 0 {
            tags.push(format!("{} todo", v.open_todos));
        }
        if s.is_background() {
            tags.push("bg".to_string());
        }
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(", "))
        };

        // The dot is the only decoration; everything else has to earn its width.
        println!("  {} {:<54} {:>7}{}", dot(v.state), title, age, tags);
        let mut detail = vec![project_name(&s.project_path)];
        if let Some(b) = branch_label(v) {
            detail.push(b);
        }
        if let Some(t) = s.term_program.as_deref() {
            detail.push(t.to_string());
        }
        println!("    {}", detail.join(" · "));
    }

    println!();
    Ok(())
}

/// The branch to show, if showing one tells the user anything.
///
/// `worktree_dirty == None` means the project is not a git repo at all, and
/// Claude Code records `gitBranch: "HEAD"` in that case — printing it would be
/// noise dressed up as information.
fn branch_label(v: &SessionView) -> Option<String> {
    v.worktree_dirty?;
    v.session
        .git_branch
        .as_deref()
        .filter(|b| !b.is_empty() && *b != "HEAD")
        .map(str::to_string)
}

fn dot(state: SessionState) -> &'static str {
    match state {
        SessionState::NeedsAction => "!",
        SessionState::Working => "*",
        SessionState::NeedsReview => "+",
        SessionState::Idle => "-",
        SessionState::Dead => "x",
        SessionState::Ended => ".",
    }
}

fn relative(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 0 {
        return "now".to_string();
    }
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn relative_time_reads_naturally() {
        assert_eq!(relative(Duration::seconds(5)), "5s ago");
        assert_eq!(relative(Duration::minutes(4)), "4m ago");
        assert_eq!(relative(Duration::hours(3)), "3h ago");
        assert_eq!(relative(Duration::days(3)), "3d ago");
        assert_eq!(relative(Duration::seconds(-5)), "now");
    }

    #[test]
    fn every_state_has_a_distinct_marker() {
        let all = [
            SessionState::NeedsAction,
            SessionState::Working,
            SessionState::NeedsReview,
            SessionState::Idle,
            SessionState::Dead,
            SessionState::Ended,
        ];
        let mut seen: Vec<&str> = all.iter().map(|s| dot(*s)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), all.len());
    }
}
