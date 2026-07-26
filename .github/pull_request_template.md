## What this changes

<!-- And why. Link an issue if there is one. -->

## Checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all` applied
- [ ] New behaviour has a test; a bug fix has a test that fails without it
- [ ] No real transcript content, prompts, or client names in code, tests, or fixtures
- [ ] Still no network calls, and still read-only with respect to Claude Code's data
