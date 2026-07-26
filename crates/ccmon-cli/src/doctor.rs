//! `ccmon doctor` — is anything about to lose your data?
//!
//! Claude Code stores transcripts in plaintext and deletes them after
//! `cleanupPeriodDays` (30 by default). The cleanup runs on every startup and
//! permanently unlinks anything older. There is no trash and no recovery, so
//! this command exists mainly to put that number in front of the user before
//! it costs them a month of history.

use anyhow::Result;
use ccmon_core::{config::Config, db, paths};
use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Claude Code's default when the setting is absent.
const DEFAULT_CLEANUP_DAYS: i64 = 30;

pub fn run(cfg: &Config) -> Result<()> {
    println!("ccmon doctor\n");

    // --- ccmon's own files ---
    let data_dir = paths::data_dir()?;
    println!("data dir      {}", data_dir.display());
    let db_path = paths::db_path()?;
    println!(
        "database      {} ({})",
        db_path.display(),
        human_size(file_size(&db_path))
    );
    let spool = paths::spool_path()?;
    if spool.exists() {
        println!(
            "spool         {} ({})",
            spool.display(),
            human_size(file_size(&spool))
        );
    } else {
        println!("spool         none yet — hooks are not installed (see `ccmon install`, M2)");
    }

    // --- discovery ---
    let discovery = cfg.all_roots();
    println!();
    if discovery.found.is_empty() {
        println!("!! No Claude Code roots found. Set claude_roots in config.toml.");
    }
    for root in &discovery.found {
        println!(
            "claude root   {}  (via {})",
            root.path.display(),
            root.source
        );
    }
    for skipped in &discovery.skipped {
        println!("  (skipped)   {}  — not present", skipped.path.display());
    }

    // --- retention, the part that actually matters ---
    let mut warned = false;
    for root in &discovery.found {
        let settings = root.settings_path();
        let configured = read_cleanup_days(&settings);
        let effective = configured.unwrap_or(DEFAULT_CLEANUP_DAYS);

        println!();
        println!("retention     {}", settings.display());
        match configured {
            Some(0) => {
                warned = true;
                println!(
                    "  !! cleanupPeriodDays is 0. Despite what the docs imply, this has been\n     \
                     reported to disable transcript persistence entirely — you get no\n     \
                     transcripts at all. Set a large finite number instead, e.g. 365."
                );
            }
            Some(d) => println!("  cleanupPeriodDays = {d}"),
            None => {
                warned = true;
                println!(
                    "  !! cleanupPeriodDays is unset, so Claude Code is using the {DEFAULT_CLEANUP_DAYS}-day default.\n     \
                     Transcripts older than {DEFAULT_CLEANUP_DAYS} days are being deleted on every startup,\n     \
                     permanently. Set it to 365 in {} to stop that.",
                    settings.display()
                );
            }
        }

        let (count, oldest) = transcript_stats(&root.projects_dir());
        match oldest {
            Some(t) => {
                let age = (Utc::now() - t).num_days();
                // Top-level transcripts only: one per session. The subagent
                // transcripts nested beneath them are not separate sessions.
                println!(
                    "  {count} session transcripts, oldest {age} days old ({})",
                    t.format("%Y-%m-%d")
                );
                if age >= effective - 2 && configured != Some(0) {
                    println!(
                        "  !! The oldest transcript is at the edge of the retention window.\n     \
                         History is being lost right now. Run `ccmon backup` first."
                    );
                }
            }
            None => println!("  {count} transcripts"),
        }
    }

    // Deletion has been reported to key off file mtime rather than real last
    // activity, so raising the setting is not a complete guarantee.
    if !cfg.archive_roots.is_empty() {
        println!("\narchives      {} configured", cfg.archive_roots.len());
        for a in &cfg.archive_roots {
            println!("  {}", a.display());
        }
    } else {
        println!(
            "\narchives      none configured. Cleanup has been reported to key off file mtime\n              \
             rather than real last activity, so raising cleanupPeriodDays is not a\n              \
             complete guarantee. `ccmon backup` keeps a copy that ccmon still ingests."
        );
    }

    // --- database contents ---
    let conn = db::open_default()?;
    let sessions: i64 = conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;
    let events: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
    let commits: i64 = conn.query_row("SELECT count(*) FROM commits", [], |r| r.get(0))?;
    println!("\nindexed       {sessions} sessions · {events} events · {commits} commits");
    if sessions == 0 {
        println!("  Run `ccmon reindex` to build the index from transcripts already on disk.");
    }

    if warned {
        println!("\nOne or more retention warnings above. Every day of delay loses history that\ncannot be recovered.");
    }
    Ok(())
}

/// Read `cleanupPeriodDays` from a settings file, tolerating anything.
fn read_cleanup_days(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("cleanupPeriodDays")?.as_i64()
}

fn transcript_stats(projects_dir: &Path) -> (usize, Option<DateTime<Utc>>) {
    let mut count = 0usize;
    let mut oldest: Option<DateTime<Utc>> = None;

    let Ok(dirs) = std::fs::read_dir(projects_dir) else {
        return (0, None);
    };
    for project in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            if file.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            count += 1;
            let modified = file
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| DateTime::from_timestamp(d.as_secs() as i64, 0));
            if let Some(m) = modified {
                oldest = Some(match oldest {
                    Some(o) if o <= m => o,
                    _ => m,
                });
            }
        }
    }
    (count, oldest)
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cleanup_days_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("settings.json");
        std::fs::write(&p, r#"{"model":"opus","cleanupPeriodDays":365}"#).unwrap();
        assert_eq!(read_cleanup_days(&p), Some(365));
    }

    #[test]
    fn absent_or_broken_settings_yield_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("settings.json");
        std::fs::write(&p, r#"{"model":"opus"}"#).unwrap();
        assert_eq!(
            read_cleanup_days(&p),
            None,
            "unset means the 30-day default"
        );

        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(read_cleanup_days(&p), None);
        assert_eq!(read_cleanup_days(Path::new("/nope/settings.json")), None);
    }

    #[test]
    fn zero_is_reported_distinctly_from_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("settings.json");
        std::fs::write(&p, r#"{"cleanupPeriodDays":0}"#).unwrap();
        assert_eq!(read_cleanup_days(&p), Some(0));
    }

    #[test]
    fn counts_transcripts_across_project_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        std::fs::create_dir_all(projects.join("-a")).unwrap();
        std::fs::create_dir_all(projects.join("-b")).unwrap();
        std::fs::write(projects.join("-a").join("1.jsonl"), "{}").unwrap();
        std::fs::write(projects.join("-a").join("notes.txt"), "x").unwrap();
        std::fs::write(projects.join("-b").join("2.jsonl"), "{}").unwrap();

        let (count, oldest) = transcript_stats(&projects);
        assert_eq!(count, 2);
        assert!(oldest.is_some());
        assert_eq!(transcript_stats(Path::new("/nope")).0, 0);
    }

    #[test]
    fn human_size_scales() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert!(human_size(5 * 1024 * 1024).ends_with("MB"));
    }
}
