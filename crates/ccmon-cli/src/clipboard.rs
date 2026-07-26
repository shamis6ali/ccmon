//! Clipboard support for `report --copy`.
//!
//! Shelling out to the platform tool rather than linking a clipboard crate:
//! the Linux ones pull in X11/Wayland system libraries that every contributor
//! would then need installed to build the CLI, for one feature.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};

/// Candidate commands in priority order.
fn candidates() -> Vec<(&'static str, Vec<&'static str>)> {
    if cfg!(target_os = "macos") {
        vec![("pbcopy", vec![])]
    } else if cfg!(target_os = "windows") {
        vec![("clip", vec![])]
    } else {
        vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ]
    }
}

pub fn copy(text: &str) -> Result<()> {
    let mut tried = Vec::new();
    for (program, args) in candidates() {
        tried.push(program);
        match try_copy(program, &args, text) {
            Ok(()) => return Ok(()),
            Err(e) => tracing::debug!(program, error = %e, "clipboard command failed"),
        }
    }
    Err(anyhow!(
        "no working clipboard command (tried {}). Pipe the report instead: ccmon report > report.md",
        tried.join(", ")
    ))
}

fn try_copy(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| anyhow!("no stdin"))?
        .write_all(text.as_bytes())?;
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("{program} exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_list_is_never_empty() {
        assert!(!candidates().is_empty());
    }

    #[test]
    fn missing_program_is_an_error_not_a_panic() {
        assert!(try_copy("ccmon-definitely-not-a-program", &[], "x").is_err());
    }
}
