//! `ccmon backup` — copy Claude Code's data somewhere its cleanup cannot reach.
//!
//! Archived roots are a first-class ingest source: add the archive to
//! `archive_roots` in `config.toml` and historical reports keep working after
//! Claude Code has pruned the originals.
//!
//! This only ever reads from the Claude roots and writes to the destination.

use anyhow::{Context, Result};
use ccmon_core::{config::Config, paths};
use chrono::Utc;
use std::path::{Path, PathBuf};

/// Subdirectories worth preserving. `projects/` is the irreplaceable one.
const SUBDIRS: &[&str] = &["projects", "tasks", "todos", "sessions"];
const FILES: &[&str] = &["settings.json", "CLAUDE.md"];

pub fn run(cfg: &Config, dest: Option<PathBuf>) -> Result<()> {
    let dest_root = match dest {
        Some(d) => d,
        None => paths::home_dir()
            .map(|h| h.join(".claude-archive"))
            .context("could not resolve a home directory for the default archive location")?,
    };

    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let discovery = cfg.all_roots();
    if discovery.found.is_empty() {
        anyhow::bail!("no Claude Code roots found; nothing to back up");
    }

    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    for (i, root) in discovery.found.iter().enumerate() {
        // Archives are themselves valid roots, so never back an archive up
        // into itself.
        if root.path.starts_with(&dest_root) {
            println!(
                "skipping {} (it is inside the archive)",
                root.path.display()
            );
            continue;
        }

        let label = if discovery.found.len() == 1 {
            stamp.clone()
        } else {
            format!("{stamp}-root{i}")
        };
        let dest = dest_root.join(&label);

        for sub in SUBDIRS {
            let from = root.path.join(sub);
            if !from.is_dir() {
                continue;
            }
            let (files, bytes) = copy_dir(&from, &dest.join(sub))?;
            total_files += files;
            total_bytes += bytes;
        }
        for file in FILES {
            let from = root.path.join(file);
            if !from.is_file() {
                continue;
            }
            std::fs::create_dir_all(&dest)?;
            std::fs::copy(&from, dest.join(file))?;
            total_files += 1;
            total_bytes += std::fs::metadata(&from).map(|m| m.len()).unwrap_or(0);
        }

        println!("archived {} -> {}", root.path.display(), dest.display());

        if !cfg.archive_roots.iter().any(|a| a == &dest) {
            println!(
                "\nTo keep reporting on this history after Claude Code prunes the originals,\n\
                 add it to {}:\n\n  archive_roots = [\"{}\"]\n",
                paths::config_path()?.display(),
                dest.display()
            );
        }
    }

    println!(
        "{total_files} files, {:.1} MB",
        total_bytes as f64 / 1_048_576.0
    );
    Ok(())
}

/// Recursive copy. Returns (files, bytes).
fn copy_dir(from: &Path, to: &Path) -> Result<(usize, u64)> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;

    for entry in std::fs::read_dir(from)
        .with_context(|| format!("reading {}", from.display()))?
        .flatten()
    {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            let (f, b) = copy_dir(&src, &dst)?;
            files += f;
            bytes += b;
        } else {
            // A transcript being appended to while we copy is expected; a
            // partial copy of one file must not abort the whole archive.
            match std::fs::copy(&src, &dst) {
                Ok(n) => {
                    files += 1;
                    bytes += n;
                }
                Err(e) => tracing::warn!(path = %src.display(), error = %e, "skipping file"),
            }
        }
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_a_tree_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("src");
        std::fs::create_dir_all(from.join("a").join("b")).unwrap();
        std::fs::write(from.join("top.jsonl"), "one").unwrap();
        std::fs::write(from.join("a").join("mid.jsonl"), "two").unwrap();
        std::fs::write(from.join("a").join("b").join("deep.jsonl"), "three").unwrap();

        let to = tmp.path().join("dest");
        let (files, bytes) = copy_dir(&from, &to).unwrap();

        assert_eq!(files, 3);
        assert_eq!(bytes, 11);
        assert_eq!(
            std::fs::read_to_string(to.join("a").join("b").join("deep.jsonl")).unwrap(),
            "three"
        );
    }

    #[test]
    fn empty_source_copies_nothing_without_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("empty");
        std::fs::create_dir_all(&from).unwrap();
        let (files, bytes) = copy_dir(&from, &tmp.path().join("out")).unwrap();
        assert_eq!((files, bytes), (0, 0));
    }
}
