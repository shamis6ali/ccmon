//! Filesystem discovery: ccmon's own data dir, and Claude Code's roots.
//!
//! Claude Code's on-disk layout is not a stable contract and users have layouts
//! we did not anticipate, so every Claude location is *probed*, never assumed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Env override for ccmon's data dir. Primarily a test seam.
pub const DATA_DIR_ENV: &str = "CCMON_DATA_DIR";

pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// ccmon's data directory. Created on demand.
///
/// - macOS:   `~/Library/Application Support/ccmon/`
/// - Linux:   `$XDG_DATA_HOME/ccmon/` or `~/.local/share/ccmon/`
/// - Windows: `%LOCALAPPDATA%\ccmon\`
pub fn data_dir() -> Result<PathBuf> {
    if let Some(over) = std::env::var_os(DATA_DIR_ENV) {
        let p = PathBuf::from(over);
        std::fs::create_dir_all(&p)
            .with_context(|| format!("creating {} from {DATA_DIR_ENV}", p.display()))?;
        return Ok(p);
    }

    let base = platform_data_base().context("could not resolve a data directory")?;
    let dir = base.join("ccmon");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn platform_data_base() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library").join("Application Support"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join("AppData").join("Local")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| home_dir().map(|h| h.join(".local").join("share")))
    }
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("ccmon.db"))
}

pub fn spool_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("events.jsonl"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("config.toml"))
}

pub fn logs_dir() -> Result<PathBuf> {
    let d = data_dir()?.join("logs");
    std::fs::create_dir_all(&d).ok();
    Ok(d)
}

/// A discovered Claude Code installation root (the directory holding
/// `projects/`, `todos/`, and `settings.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeRoot {
    pub path: PathBuf,
    /// Why this root was included; surfaced by `ccmon doctor`.
    pub source: String,
}

impl ClaudeRoot {
    pub fn projects_dir(&self) -> PathBuf {
        self.path.join("projects")
    }
    pub fn todos_dir(&self) -> PathBuf {
        self.path.join("todos")
    }
    pub fn settings_path(&self) -> PathBuf {
        self.path.join("settings.json")
    }
}

/// A candidate that was probed and rejected, for `doctor` output.
#[derive(Debug, Clone)]
pub struct SkippedRoot {
    pub path: PathBuf,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub found: Vec<ClaudeRoot>,
    pub skipped: Vec<SkippedRoot>,
}

/// Probe every known Claude Code location plus any user-configured extras.
///
/// `extra` comes from `config.toml`'s `claude_roots` and from archive dirs;
/// archived transcript trees are a first-class ingest source so historical
/// reports keep working after Claude Code prunes the originals.
pub fn discover_claude_dirs(extra: &[PathBuf]) -> Discovery {
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();

    for p in extra {
        candidates.push((p.clone(), "config: claude_roots".to_string()));
    }

    if let Some(v) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        candidates.push((PathBuf::from(v), "$CLAUDE_CONFIG_DIR".to_string()));
    }

    if let Some(home) = home_dir() {
        candidates.push((home.join(".claude"), "~/.claude".to_string()));

        #[cfg(target_os = "macos")]
        candidates.push((
            home.join("Library")
                .join("Application Support")
                .join("Claude"),
            "~/Library/Application Support/Claude".to_string(),
        ));

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        candidates.push((
            home.join(".config").join("Claude"),
            "~/.config/Claude".to_string(),
        ));
    }

    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push((
            PathBuf::from(appdata).join("Claude"),
            "%APPDATA%\\Claude".to_string(),
        ));
    }

    let mut out = Discovery::default();
    let mut seen: Vec<PathBuf> = Vec::new();

    for (path, source) in candidates {
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.contains(&canon) {
            continue;
        }
        // A root is real if it has a projects/ dir or a settings.json. An empty
        // ~/.claude that only holds an ide/ socket dir is not worth ingesting.
        let looks_real = path.join("projects").is_dir() || path.join("settings.json").is_file();
        if looks_real {
            seen.push(canon.clone());
            out.found.push(ClaudeRoot { path, source });
        } else {
            out.skipped.push(SkippedRoot { path, source });
        }
    }

    for r in &out.found {
        tracing::debug!(path = %r.path.display(), source = %r.source, "claude root found");
    }
    for s in &out.skipped {
        tracing::debug!(path = %s.path.display(), source = %s.source, "claude root skipped");
    }

    out
}

/// Resolve a path to one canonical spelling, so two of them can be compared.
///
/// Three things make raw path strings unsafe to compare:
///
/// - **Symlinks.** `git rev-parse --show-toplevel` always returns a fully
///   resolved path, but a transcript records whatever string the session used.
///   On macOS `/tmp` is a symlink to `/private/tmp`, so a session working under
///   `/tmp` produces files that share no prefix with their own repo root.
/// - **Windows verbatim prefixes.** `canonicalize` returns `\\?\C:\…` there,
///   while git returns `C:/…`. Canonicalising alone would swap one mismatch
///   for another, so the prefix is stripped.
///
/// Separators are deliberately *not* touched. Git reports forward slashes on
/// Windows and the filesystem reports backslashes, but `std::path` treats both
/// as separators there, so comparing components already copes — and rewriting
/// them would corrupt a path that could not be resolved.
///
/// Everything that compares a file against a project root must pass both sides
/// through here first. Unresolvable paths are returned unchanged, which is
/// normal for a file that was edited and later deleted.
pub fn normalize(path: &str) -> String {
    // A path that cannot be resolved is returned exactly as recorded. Rewriting
    // it would be inventing information about a file we could not even find.
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return path.to_string();
    };
    let resolved = resolved.display().to_string();

    #[cfg(windows)]
    {
        // `\\?\C:\x` -> `C:\x`, and UNC `\\?\UNC\srv\share` -> `\\srv\share`.
        resolved
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| resolved.strip_prefix(r"\\?\").map(str::to_string))
            .unwrap_or(resolved)
    }
    #[cfg(not(windows))]
    {
        resolved
    }
}

/// Is `file` inside `dir`?
///
/// Compares path *components* rather than string prefixes, so `/a/bc` is not
/// treated as living inside `/a/b`. Both sides should already be `normalize`d.
pub fn is_inside(file: &str, dir: &str) -> bool {
    Path::new(file).starts_with(Path::new(dir))
}

/// `file` relative to `dir`, or `None` when it is not inside it.
pub fn relative_within(file: &str, dir: &str) -> Option<String> {
    Path::new(file)
        .strip_prefix(Path::new(dir))
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|s| !s.is_empty())
}

/// Probe only the given paths, skipping every built-in candidate.
///
/// Backs `only_configured_roots`.
pub fn probe_only(paths: &[PathBuf]) -> Discovery {
    let mut out = Discovery::default();
    for path in paths {
        if path.join("projects").is_dir() || path.join("settings.json").is_file() {
            out.found.push(ClaudeRoot {
                path: path.clone(),
                source: "config: claude_roots (exclusive)".to_string(),
            });
        } else {
            out.skipped.push(SkippedRoot {
                path: path.clone(),
                source: "config: claude_roots (exclusive)".to_string(),
            });
        }
    }
    out
}

/// Claude Code names project dirs by mangling the absolute cwd. The mangling is
/// lossy (both `/` and `.` become `-`), so we only ever use this to *derive a
/// display slug from a known path*, never to recover a path from a slug.
pub fn slug_for_path(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '.' | '_' => '-',
            c => c,
        })
        .collect()
}

/// Render an absolute path with `~` for the home dir, for report output.
pub fn abbreviate_home(path: &str) -> String {
    if let Some(home) = home_dir() {
        let home = home.to_string_lossy().to_string();
        if path == home {
            return "~".to_string();
        }
        if let Some(rest) = path.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
    }
    path.to_string()
}

/// Last path component, for a human-facing project name.
pub fn project_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_mangling() {
        assert_eq!(
            slug_for_path("/home/dev/src/prompt-master"),
            "-home-dev-src-prompt-master"
        );
    }

    #[test]
    fn project_name_is_last_component() {
        assert_eq!(project_name("/a/b/ccmon"), "ccmon");
        assert_eq!(project_name("/"), "/");
    }

    #[test]
    fn containment_compares_components_not_string_prefixes() {
        // The bug a string prefix would introduce: a sibling directory whose
        // name merely starts with the project's name would swallow its files.
        assert!(is_inside("/a/b/src/x.ts", "/a/b"));
        assert!(!is_inside("/a/bc/src/x.ts", "/a/b"));
        assert!(!is_inside("/other/x.ts", "/a/b"));
        assert!(is_inside("/a/b", "/a/b"), "a project contains itself");
    }

    #[test]
    fn relative_within_strips_the_project_root() {
        assert_eq!(
            relative_within("/a/b/src/x.ts", "/a/b").as_deref(),
            Some("src/x.ts")
        );
        // Not inside, and the root itself, both yield nothing to render.
        assert_eq!(relative_within("/a/bc/x.ts", "/a/b"), None);
        assert_eq!(relative_within("/a/b", "/a/b"), None);
    }

    #[test]
    fn normalize_leaves_an_unresolvable_path_alone() {
        // Edited then deleted is normal; it must not vanish from the report.
        // Byte-for-byte what was recorded: rewriting a path we could not even
        // resolve would be inventing information about it.
        for missing in ["/definitely/not/here/deleted.rs", r"C:\nope\gone.rs"] {
            assert_eq!(normalize(missing), missing);
        }
    }

    #[test]
    fn discovery_skips_nonexistent() {
        let d = discover_claude_dirs(&[PathBuf::from("/definitely/not/here/ccmon-test")]);
        assert!(d.skipped.iter().any(|s| s.path.ends_with("ccmon-test")));
    }
}
