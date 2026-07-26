//! Transcript (`~/.claude/projects/<slug>/<uuid>.jsonl`) parsing.
//!
//! **The transcript format is not a documented stable contract.** It has
//! already changed once under us: the session title used to arrive as a
//! `{"type":"summary","summary":...}` line and now arrives as
//! `{"type":"ai-title","aiTitle":...}`. Both are accepted here, and so is
//! anything else that shows up — every field is optional, unknown keys are
//! captured rather than rejected, and a line that fails to parse is skipped
//! with a debug log. A bad line must never fail a file, and a bad file must
//! never fail a run.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::{max_opt, min_opt, parse_ts};

/// Tools whose invocation means a file was written.
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// First-prompt candidates that are machinery, not something the user typed.
const SYNTHETIC_PREFIXES: &[&str] = &[
    "<command-name>",
    "<command-message>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<system-reminder>",
    "<user-memory-input>",
    "Caveat: The messages below",
    "This session is being continued from a previous conversation",
];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    version: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    /// Legacy title line.
    summary: Option<String>,
    /// Current title line. This is the string Claude Code puts in the terminal
    /// window title, which is what lets the user find the window.
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    #[serde(rename = "lastPrompt")]
    last_prompt: Option<String>,
    message: Option<Value>,
    /// Present on user lines that are carrying a tool result rather than a prompt.
    #[serde(rename = "toolUseResult")]
    tool_use_result: Option<Value>,
    #[serde(flatten)]
    #[allow(dead_code)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedTranscript {
    pub path: PathBuf,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    /// Session title: `ai-title` if present, else the legacy `summary` line.
    pub summary: Option<String>,
    /// Verbatim first human prompt, truncated to 2000 chars.
    pub first_prompt: Option<String>,
    /// Most recent prompt, when the transcript records one explicitly.
    pub last_prompt: Option<String>,
    pub first_ts: Option<DateTime<Utc>>,
    pub last_ts: Option<DateTime<Utc>>,
    pub last_user_ts: Option<DateTime<Utc>>,
    pub last_assistant_ts: Option<DateTime<Utc>>,
    pub files: BTreeMap<String, i64>,
    pub tool_calls: i64,
    pub lines_total: usize,
    pub lines_skipped: usize,
}

/// Max stored length of a verbatim prompt.
const PROMPT_MAX: usize = 2000;

pub fn parse_file(path: &Path) -> std::io::Result<ParsedTranscript> {
    let file = std::fs::File::open(path)?;
    let mut out = parse_reader(BufReader::new(file));
    out.path = path.to_path_buf();
    if out.session_id.is_none() {
        // The filename is the session uuid; fall back to it when no line
        // carried a sessionId.
        out.session_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| looks_like_uuid(s));
    }
    Ok(out)
}

pub fn parse_reader<R: Read>(mut reader: BufReader<R>) -> ParsedTranscript {
    let mut out = ParsedTranscript::default();
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);

    loop {
        buf.clear();
        // Read raw bytes rather than `lines()`: a transcript containing invalid
        // UTF-8 must degrade to lossy text, not abort the file.
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "transcript read error, stopping file");
                break;
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.lines_total += 1;

        let raw: RawLine = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                out.lines_skipped += 1;
                tracing::debug!(error = %e, "skipping unparseable transcript line");
                continue;
            }
        };
        absorb(&mut out, raw);
    }

    if let Some(p) = out.first_prompt.as_mut() {
        truncate_chars(p, PROMPT_MAX);
    }
    if let Some(p) = out.last_prompt.as_mut() {
        truncate_chars(p, PROMPT_MAX);
    }
    out
}

fn absorb(out: &mut ParsedTranscript, raw: RawLine) {
    if out.session_id.is_none() {
        out.session_id = raw.session_id.clone();
    }
    if let Some(cwd) = raw.cwd.as_ref() {
        // Later lines win: a session can move, and the newest cwd is the one
        // that matters. Never reverse-engineer this from the directory slug,
        // which is lossy.
        out.cwd = Some(cwd.clone());
    }
    if let Some(b) = raw.git_branch.as_ref().filter(|b| !b.is_empty()) {
        out.git_branch = Some(b.clone());
    }
    if let Some(v) = raw.version.as_ref() {
        out.version = Some(v.clone());
    }

    // Title: current format first, legacy second. Most recent wins.
    if let Some(t) = raw.ai_title.as_ref().or(raw.summary.as_ref()) {
        let t = t.trim();
        if !t.is_empty() {
            out.summary = Some(t.to_string());
        }
    }
    if let Some(p) = raw.last_prompt.as_ref() {
        let p = p.trim();
        if !p.is_empty() {
            out.last_prompt = Some(p.to_string());
        }
    }

    let ts = raw.timestamp.as_deref().and_then(parse_ts);
    if let Some(ts) = ts {
        out.first_ts = min_opt(out.first_ts, Some(ts));
        out.last_ts = max_opt(out.last_ts, Some(ts));
    }

    let kind = raw.kind.as_deref().unwrap_or("");
    let role = raw
        .message
        .as_ref()
        .and_then(|m| m.get("role"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let is_user = kind == "user" || role == "user";
    let is_assistant = kind == "assistant" || role == "assistant";
    let is_sidechain = raw.is_sidechain.unwrap_or(false);
    let carries_tool_result = raw.tool_use_result.is_some();

    if is_user && !carries_tool_result {
        out.last_user_ts = max_opt(out.last_user_ts, ts);
    }
    if is_assistant {
        out.last_assistant_ts = max_opt(out.last_assistant_ts, ts);
    }

    let content = raw.message.as_ref().and_then(|m| m.get("content"));

    // First human prompt. Sidechain messages are subagent instructions, and
    // meta / tool-result messages are machinery; none of them is what the user
    // typed.
    if is_user
        && out.first_prompt.is_none()
        && !is_sidechain
        && !raw.is_meta.unwrap_or(false)
        && !carries_tool_result
    {
        if let Some(text) = content.and_then(plain_text) {
            let text = text.trim();
            if !text.is_empty() && !is_synthetic(text) {
                out.first_prompt = Some(text.to_string());
            }
        }
    }

    if let Some(Value::Array(blocks)) = content {
        for block in blocks {
            let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if btype != "tool_use" {
                continue;
            }
            out.tool_calls += 1;
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !EDIT_TOOLS.contains(&name) {
                continue;
            }
            let input = block.get("input");
            let file = input
                .and_then(|i| i.get("file_path"))
                .or_else(|| input.and_then(|i| i.get("notebook_path")))
                .and_then(|v| v.as_str());
            if let Some(f) = file.filter(|f| !f.is_empty()) {
                *out.files.entry(f.to_string()).or_insert(0) += 1;
            }
        }
    }
}

/// Extract prompt text from a message content field.
///
/// Content is either a bare string or an array of typed blocks. An array
/// containing a `tool_result` is a tool response wearing a user message's
/// clothes, so it yields nothing.
fn plain_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                let t = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if t == "tool_result" {
                    return None;
                }
                if t == "text" {
                    if let Some(s) = item.get("text").and_then(|v| v.as_str()) {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(s);
                    }
                }
            }
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

fn is_synthetic(text: &str) -> bool {
    SYNTHETIC_PREFIXES.iter().any(|p| text.starts_with(p))
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Truncate on a char boundary, in place.
pub fn truncate_chars(s: &mut String, max: usize) {
    if s.chars().count() <= max {
        return;
    }
    let end = s
        .char_indices()
        .nth(max)
        .map(|(i, _)| i)
        .unwrap_or_else(|| s.len());
    s.truncate(end);
}

/// Truncate to `max` chars on a word boundary, appending an ellipsis.
pub fn truncate_words(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut cut: String = s.chars().take(max).collect();
    if let Some(idx) = cut.rfind(char::is_whitespace) {
        // Only back up to a word boundary if it doesn't gut the string.
        if idx > max / 2 {
            cut.truncate(idx);
        }
    }
    format!("{}…", cut.trim_end())
}

/// Collapse newlines so a verbatim prompt stays on one report line.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(s: &str) -> ParsedTranscript {
        parse_reader(BufReader::new(Cursor::new(s.to_string())))
    }

    #[test]
    fn parses_current_format_with_ai_title() {
        let t = parse(concat!(
            r#"{"type":"mode","mode":"normal","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"port the replit site"},"timestamp":"2026-06-28T21:53:20.603Z","cwd":"/Users/x/proj","sessionId":"s1","gitBranch":"main"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Port portfolio site","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/Users/x/proj/a.ts"}}]},"timestamp":"2026-06-28T22:00:00.000Z"}"#,
        ));
        assert_eq!(t.session_id.as_deref(), Some("s1"));
        assert_eq!(t.summary.as_deref(), Some("Port portfolio site"));
        assert_eq!(t.first_prompt.as_deref(), Some("port the replit site"));
        assert_eq!(t.cwd.as_deref(), Some("/Users/x/proj"));
        assert_eq!(t.git_branch.as_deref(), Some("main"));
        assert_eq!(t.files.get("/Users/x/proj/a.ts"), Some(&1));
        assert_eq!(t.tool_calls, 1);
        assert_eq!(t.lines_skipped, 0);
    }

    #[test]
    fn accepts_legacy_summary_lines() {
        let t = parse(r#"{"type":"summary","summary":"Old style title","leafUuid":"x"}"#);
        assert_eq!(t.summary.as_deref(), Some("Old style title"));
    }

    #[test]
    fn most_recent_title_wins() {
        let t = parse(concat!(
            r#"{"type":"ai-title","aiTitle":"First"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Second"}"#,
        ));
        assert_eq!(t.summary.as_deref(), Some("Second"));
    }

    #[test]
    fn tool_result_user_messages_are_not_prompts() {
        let t = parse(concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]},"toolUseResult":{"x":1},"timestamp":"2026-01-01T00:00:00.000Z"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"the real first prompt"},"timestamp":"2026-01-01T00:01:00.000Z"}"#,
        ));
        assert_eq!(t.first_prompt.as_deref(), Some("the real first prompt"));
    }

    #[test]
    fn sidechain_and_synthetic_prompts_are_skipped() {
        let t = parse(concat!(
            r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"subagent instructions"},"timestamp":"2026-01-01T00:00:00.000Z"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"},"timestamp":"2026-01-01T00:00:30.000Z"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"actual question"},"timestamp":"2026-01-01T00:01:00.000Z"}"#,
        ));
        assert_eq!(t.first_prompt.as_deref(), Some("actual question"));
    }

    #[test]
    fn garbage_truncated_and_reordered_lines_never_panic() {
        let t = parse(concat!(
            "not json at all\n",
            r#"{"type":"user","message":{"role":"user","content":"#,
            "\n",
            "\n",
            r#"{"type":"assistant","timestamp":"2026-01-02T00:00:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"late prompt"},"timestamp":"2026-01-01T00:00:00.000Z"}"#,
            "\n",
            r#"{"type":"totally-unknown-future-type","weird":{"nested":[1,2,3]}}"#,
        ));
        assert_eq!(t.lines_skipped, 2);
        assert_eq!(t.first_prompt.as_deref(), Some("late prompt"));
        // Timestamps are derived by min/max, never by line order.
        assert_eq!(
            t.first_ts.map(|d| d.to_rfc3339()),
            parse_ts("2026-01-01T00:00:00.000Z").map(|d| d.to_rfc3339())
        );
        assert_eq!(
            t.last_ts.map(|d| d.to_rfc3339()),
            parse_ts("2026-01-02T00:00:00.000Z").map(|d| d.to_rfc3339())
        );
        // A tool_use with no file_path counts as activity but edits nothing.
        assert_eq!(t.tool_calls, 1);
        assert!(t.files.is_empty());
    }

    #[test]
    fn invalid_utf8_degrades_instead_of_failing() {
        let mut bytes = br#"{"type":"user","message":{"role":"user","content":"caf"#.to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(br#""},"timestamp":"2026-01-01T00:00:00.000Z"}"#);
        bytes.push(b'\n');
        bytes.extend_from_slice(br#"{"type":"ai-title","aiTitle":"ok"}"#);
        let t = parse_reader(BufReader::new(Cursor::new(bytes)));
        assert_eq!(t.summary.as_deref(), Some("ok"));
    }

    #[test]
    fn edit_counts_accumulate_per_file() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a.ts"}},{"type":"tool_use","name":"Write","input":{"file_path":"/a.ts"}},{"type":"tool_use","name":"Read","input":{"file_path":"/b.ts"}}]}}"#;
        let t = parse(line);
        assert_eq!(t.files.get("/a.ts"), Some(&2));
        assert_eq!(t.files.get("/b.ts"), None, "Read is not an edit");
        assert_eq!(t.tool_calls, 3);
    }

    #[test]
    fn notebook_edits_use_notebook_path() {
        let t = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"NotebookEdit","input":{"notebook_path":"/n.ipynb"}}]}}"#,
        );
        assert_eq!(t.files.get("/n.ipynb"), Some(&1));
    }

    #[test]
    fn truncate_words_respects_boundaries() {
        let s = "the quick brown fox jumps over the lazy dog";
        let out = truncate_words(s, 20);
        assert!(out.ends_with('…'));
        assert!(out.len() <= 25);
        assert_eq!(truncate_words("short", 20), "short");
    }

    #[test]
    fn one_line_collapses_whitespace() {
        assert_eq!(one_line("a\n\n  b\tc  "), "a b c");
    }

    #[test]
    fn long_prompts_truncate_at_2000_chars() {
        let long = "x".repeat(5000);
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{long}"}},"timestamp":"2026-01-01T00:00:00.000Z"}}"#
        );
        let t = parse(&line);
        assert_eq!(t.first_prompt.unwrap().chars().count(), PROMPT_MAX);
    }
}
