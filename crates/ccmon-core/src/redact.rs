//! Redacting credentials out of report output.
//!
//! The report reproduces the first prompt of a session verbatim, and its whole
//! purpose is to be pasted into a chat window. People paste API keys into
//! prompts — that is simply true — so without this the tool would quietly
//! forward a credential from a private transcript into a third party.
//!
//! Deliberately conservative. Only shapes that are unambiguously credentials
//! are matched: no entropy heuristics, no "looks random" scoring. A false
//! positive silently corrupts a work report, and the cost of a miss is bounded
//! by the fact that the user still reads what they paste. This reduces
//! exposure; it is not a guarantee, and the README says so.

use std::sync::OnceLock;

use regex::Regex;

/// What a redacted value is replaced with.
pub const MASK: &str = "[redacted]";

/// Patterns with a fixed, vendor-published prefix. Each is anchored on that
/// prefix rather than on general shape, which is what keeps false positives
/// near zero.
const PATTERNS: &[&str] = &[
    // OpenAI and compatible: sk-…, sk-proj-…
    r"\bsk-[A-Za-z0-9_-]{16,}",
    // Anthropic
    r"\bsk-ant-[A-Za-z0-9_-]{16,}",
    // GitHub: ghp_ (classic), gho_/ghu_/ghs_/ghr_, github_pat_
    r"\bgh[pousr]_[A-Za-z0-9]{16,}",
    r"\bgithub_pat_[A-Za-z0-9_]{20,}",
    // AWS access key id, and the secret when it is labelled
    r"\bAKIA[0-9A-Z]{16}\b",
    r"\bASIA[0-9A-Z]{16}\b",
    // Google API keys. Real ones are AIza plus exactly 35 characters, but the
    // bound is open-ended on purpose: matching a fixed length and then
    // requiring a word boundary means a slightly longer token matches nothing
    // at all, which is the worst outcome — a near-miss leaves the secret in
    // the report. Over-matching only ever masks more.
    r"\bAIza[A-Za-z0-9_-]{35,}",
    // Slack
    r"\bxox[abposr]-[A-Za-z0-9-]{10,}",
    // Stripe live keys only; test keys are not secrets worth mangling reports for
    r"\b[rs]k_live_[A-Za-z0-9]{16,}",
    // Private key blocks
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
    // JWTs
    r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
    // Bearer tokens and common assignments, where the name says it is a secret
    r"(?i)\b(?:bearer|authorization:\s*bearer)\s+[A-Za-z0-9._~+/-]{20,}=*",
    r"(?i)\b(?:api[_-]?key|secret|password|passwd|token|access[_-]?key)\s*[:=]\s*['\x22]?[A-Za-z0-9._~+/-]{12,}['\x22]?",
];

fn patterns() -> &'static Vec<Regex> {
    static COMPILED: OnceLock<Vec<Regex>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        PATTERNS
            .iter()
            .filter_map(|p| match Regex::new(p) {
                Ok(r) => Some(r),
                Err(e) => {
                    // A bad pattern must degrade to "this one does not run",
                    // never to a panic in the middle of a report.
                    tracing::error!(pattern = p, error = %e, "invalid redaction pattern");
                    None
                }
            })
            .collect()
    })
}

/// Replace anything that looks unambiguously like a credential.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for re in patterns() {
        if re.is_match(&out) {
            out = re.replace_all(&out, MASK).into_owned();
        }
    }
    out
}

/// Whether redaction changed anything, for reporting a count to the user.
pub fn contains_secret(text: &str) -> bool {
    patterns().iter().any(|re| re.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redacted(s: &str) -> bool {
        redact(s).contains(MASK)
    }

    #[test]
    fn masks_vendor_prefixed_keys() {
        for secret in [
            "sk-abcdefghijklmnopqrstuvwx1234",
            "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv",
            "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz012345",
            "github_pat_11ABCDEFG0abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
            "AIzaSyD-1234567890abcdefghijklmnopqrstuv",
            "xoxb-123456789012-abcdefghijkl",
            "sk_live_abcdefghijklmnopqrst",
        ] {
            assert!(redacted(secret), "should redact: {secret}");
            assert!(!redact(secret).contains(secret), "leaked: {secret}");
        }
    }

    #[test]
    fn masks_jwts_and_private_keys() {
        assert!(redacted(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r"
        ));
        assert!(redacted("-----BEGIN RSA PRIVATE KEY-----"));
    }

    #[test]
    fn masks_labelled_assignments() {
        for s in [
            "api_key=abcdef1234567890",
            "API_KEY: 'abcdef1234567890'",
            "password = hunter2hunter2hunter2",
            "Authorization: Bearer abcdefghijklmnopqrstuvwx",
            "token: abcdefghijklmnopqrst",
        ] {
            assert!(redacted(s), "should redact: {s}");
        }
    }

    #[test]
    fn leaves_ordinary_prose_alone() {
        // False positives silently corrupt a work report, so this is the test
        // that matters most.
        for s in [
            "update the pricing section and swap the hero copy",
            "the cloud run deploy is failing on the build step, figure out why",
            "fix the auth flow so tokens refresh properly",
            "rename secret-santa.md to gift-exchange.md",
            "we need a password reset screen",
            "port the replit site to Next.js 16 and deploy to Cloud Run",
            "commit a3f21e9 broke the pricing grid",
            "see https://example.com/docs/getting-started for the api key docs",
        ] {
            assert_eq!(redact(s), s, "false positive on: {s}");
        }
    }

    #[test]
    fn redacts_a_secret_embedded_in_a_sentence() {
        let prompt = "deploy this using sk-abcdefghijklmnopqrstuvwx1234 as the key";
        let out = redact(prompt);
        assert!(out.starts_with("deploy this using "));
        assert!(out.ends_with(" as the key"));
        assert!(!out.contains("sk-abcdef"));
    }

    #[test]
    fn handles_several_secrets_in_one_string() {
        let out = redact("first ghp_AbCdEfGhIjKlMnOpQrStUvWxYz012345 then AKIAIOSFODNN7EXAMPLE");
        assert_eq!(out.matches(MASK).count(), 2, "{out}");
    }

    #[test]
    fn detection_matches_replacement() {
        assert!(contains_secret("AKIAIOSFODNN7EXAMPLE"));
        assert!(!contains_secret("just some ordinary text"));
    }

    #[test]
    fn empty_and_huge_inputs_are_safe() {
        assert_eq!(redact(""), "");
        let big = "word ".repeat(20_000);
        assert_eq!(redact(&big), big);
    }
}
