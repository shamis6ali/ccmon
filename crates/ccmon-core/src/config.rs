//! `config.toml` in the ccmon data dir. Every field has a default, so a missing
//! or partial file is always valid.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_active_window_secs() -> i64 {
    300
}
fn default_stale_after_days() -> i64 {
    3
}
fn default_include_ended_in_report() -> bool {
    true
}
fn default_git_timeout_secs() -> u64 {
    5
}
fn default_git_cache_ttl_secs() -> u64 {
    30
}
fn default_commit_grace_secs() -> i64 {
    300
}
fn default_git_lookback_days() -> i64 {
    120
}
fn default_notifications_enabled() -> bool {
    true
}
fn default_spool_max_bytes() -> u64 {
    32 * 1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Extra Claude Code roots to probe beyond the built-in candidates.
    pub claude_roots: Vec<PathBuf>,
    /// Use `claude_roots` exclusively and skip auto-discovery entirely.
    ///
    /// Off by default: probing is what copes with layouts we did not
    /// anticipate. Turn it on to point ccmon at one specific tree — an archive
    /// you are reporting on, or a fixture under test.
    pub only_configured_roots: bool,
    /// Archived copies of Claude roots (see `ccmon backup`). Ingested the same
    /// way, so reports keep working after Claude Code prunes the originals.
    pub archive_roots: Vec<PathBuf>,
    /// A session with an open turn and no activity inside this window is a
    /// hung turn, not working.
    pub active_window_secs: i64,
    /// Staleness threshold. Staleness is a flag, never a state.
    pub stale_after_days: i64,
    /// PreToolUse runs *before* the tool, so its latency is directly
    /// perceptible and PostToolUse already gives the activity signal.
    /// Off by default, deliberately.
    pub track_pre_tool_use: bool,
    /// Substrings; any project path containing one is ignored entirely.
    pub exclude_projects: Vec<String>,
    /// Whether cleanly-ENDED sessions appear in reports.
    pub include_ended_in_report: bool,
    /// Mask credentials in report output.
    ///
    /// The report reproduces prompts verbatim and is designed to be pasted
    /// into a chat, so a key pasted into a prompt would otherwise be forwarded
    /// to a third party. On by default; turning it off is a deliberate act.
    pub redact_secrets: bool,
    pub notifications_enabled: bool,
    pub git_timeout_secs: u64,
    pub git_cache_ttl_secs: u64,
    /// Commits landing shortly after a session's last event still belong to it.
    pub commit_grace_secs: i64,
    /// How far back `git log` reaches when a session has no known start.
    pub git_lookback_days: i64,
    pub spool_max_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            claude_roots: Vec::new(),
            only_configured_roots: false,
            archive_roots: Vec::new(),
            active_window_secs: default_active_window_secs(),
            stale_after_days: default_stale_after_days(),
            track_pre_tool_use: false,
            exclude_projects: Vec::new(),
            include_ended_in_report: default_include_ended_in_report(),
            redact_secrets: true,
            notifications_enabled: default_notifications_enabled(),
            git_timeout_secs: default_git_timeout_secs(),
            git_cache_ttl_secs: default_git_cache_ttl_secs(),
            commit_grace_secs: default_commit_grace_secs(),
            git_lookback_days: default_git_lookback_days(),
            spool_max_bytes: default_spool_max_bytes(),
        }
    }
}

impl Config {
    /// Load from the ccmon data dir. A missing file yields defaults; a
    /// malformed file is reported rather than silently ignored, because a
    /// config the user wrote and we quietly dropped is worse than an error.
    pub fn load() -> Result<Self> {
        let path = crate::paths::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    /// Write a fully-commented default config if none exists yet.
    pub fn write_default_if_missing() -> Result<Option<PathBuf>> {
        let path = crate::paths::config_path()?;
        if path.exists() {
            return Ok(None);
        }
        std::fs::write(&path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(Some(path))
    }

    /// Set individual keys in `config.toml`, preserving everything else.
    ///
    /// Editing in place rather than re-serialising the whole struct: the file
    /// ships with explanatory comments and the user may have added their own,
    /// and silently deleting someone's comments because they toggled a
    /// checkbox is not acceptable.
    ///
    /// `values` are TOML fragments (`true`, `7`, `"text"`).
    pub fn patch(values: &[(&str, String)]) -> Result<()> {
        let path = crate::paths::config_path()?;
        let existing = if path.exists() {
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
        } else {
            DEFAULT_CONFIG_TOML.to_string()
        };

        let mut doc = existing
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))?;

        for (key, value) in values {
            let item = value
                .parse::<toml_edit::Item>()
                .with_context(|| format!("invalid value for {key}: {value}"))?;
            doc[*key] = item;
        }

        std::fs::write(&path, doc.to_string())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// All roots to ingest: discovered Claude roots plus configured archives.
    pub fn all_roots(&self) -> crate::paths::Discovery {
        let mut extra = self.claude_roots.clone();
        extra.extend(self.archive_roots.iter().cloned());
        if self.only_configured_roots {
            return crate::paths::probe_only(&extra);
        }
        crate::paths::discover_claude_dirs(&extra)
    }

    pub fn is_excluded(&self, project_path: &str) -> bool {
        self.exclude_projects
            .iter()
            .any(|needle| !needle.is_empty() && project_path.contains(needle.as_str()))
    }
}

pub const DEFAULT_CONFIG_TOML: &str = r#"# ccmon configuration
# Every value here is optional; deleting a line restores the default.

# Extra Claude Code roots to probe, beyond $CLAUDE_CONFIG_DIR and ~/.claude.
claude_roots = []

# Use claude_roots exclusively and skip auto-discovery. Probing is what copes
# with layouts we did not anticipate, so leave this off unless you deliberately
# want ccmon looking at one specific tree.
only_configured_roots = false

# Archived copies of Claude roots (see `ccmon backup`). Ingested identically,
# so historical reports survive Claude Code's own transcript cleanup.
archive_roots = []

# An open turn with no activity for this long is a hung turn, not "working".
active_window_secs = 300

# Sessions untouched for this many days get a "stale" flag. Staleness is a flag
# and not a state, because a stale NEEDS_REVIEW and a stale DEAD are genuinely
# different problems.
stale_after_days = 3

# PreToolUse fires *before* each tool call, so its latency is directly
# perceptible in every Claude Code session, and PostToolUse already provides the
# same activity signal. Leave this off unless you specifically need it.
track_pre_tool_use = false

# Substring matches; any project path containing one is ignored entirely.
exclude_projects = []

# Include cleanly-ended sessions in `ccmon report`.
include_ended_in_report = true

# Mask things that are unambiguously credentials (sk-…, ghp_…, AKIA…, JWTs,
# "api_key = …") in report output. The report quotes your prompts verbatim and
# is meant to be pasted into a chat, so leave this on unless you have a reason.
redact_secrets = true

# Desktop notifications when a session enters NEEDS_ACTION.
notifications_enabled = true

# Git is shelled out to; a slow or broken repo must never hang the app.
git_timeout_secs = 5
git_cache_ttl_secs = 30

# A commit landing this soon after a session's last event still counts as that
# session's work.
commit_grace_secs = 300

# How far back to scan git history for projects with no known session start.
git_lookback_days = 120

# Rotate events.jsonl past this size.
spool_max_bytes = 33554432
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.active_window_secs, 300);
        assert_eq!(cfg.stale_after_days, 3);
        assert!(!cfg.track_pre_tool_use);
        assert!(cfg.include_ended_in_report);
    }

    #[test]
    fn shipped_default_toml_parses_and_matches_defaults() {
        let cfg: Config = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        let d = Config::default();
        assert_eq!(cfg.active_window_secs, d.active_window_secs);
        assert_eq!(cfg.stale_after_days, d.stale_after_days);
        assert_eq!(cfg.spool_max_bytes, d.spool_max_bytes);
        assert_eq!(cfg.commit_grace_secs, d.commit_grace_secs);
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let cfg: Config = toml::from_str("stale_after_days = 10").unwrap();
        assert_eq!(cfg.stale_after_days, 10);
        assert_eq!(cfg.active_window_secs, 300);
    }

    #[test]
    fn patch_preserves_comments_and_other_keys() {
        let tmp = tempfile::tempdir().unwrap();
        // `patch` writes to the resolved data dir, so point it at a temp dir.
        std::env::set_var(crate::paths::DATA_DIR_ENV, tmp.path());

        let path = crate::paths::config_path().unwrap();
        std::fs::write(
            &path,
            "# a comment the user wrote\nstale_after_days = 3\nnotifications_enabled = true\n",
        )
        .unwrap();

        Config::patch(&[
            ("stale_after_days", "10".into()),
            ("notifications_enabled", "false".into()),
        ])
        .unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("# a comment the user wrote"),
            "comments must survive: {after}"
        );
        assert!(after.contains("stale_after_days = 10"), "{after}");
        assert!(after.contains("notifications_enabled = false"), "{after}");

        std::env::remove_var(crate::paths::DATA_DIR_ENV);
    }

    #[test]
    fn exclusions_are_substring_matches() {
        let cfg = Config {
            exclude_projects: vec!["/scratch".into()],
            ..Default::default()
        };
        assert!(cfg.is_excluded("/Users/x/scratch/thing"));
        assert!(!cfg.is_excluded("/Users/x/work/thing"));
    }
}
