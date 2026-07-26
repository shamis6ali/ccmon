# Contributing

Thanks for looking. This is a small tool with a deliberately narrow scope —
please read [Scope](#scope) before opening a large PR, so neither of us wastes
an afternoon.

## Getting set up

Requires Rust 1.77.2+ and, for the desktop app, Node 18+.

```sh
git clone https://github.com/shamis6ali/ccmon
cd ccmon

cargo test --workspace          # 130+ tests, no fixtures needed
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

The CLI needs nothing else:

```sh
cargo run -p ccmon-cli -- doctor
cargo run -p ccmon-cli -- ls
```

The desktop app needs the frontend built first:

```sh
cd ui && npm install && cd ..
cd crates/ccmon-app && cargo tauri dev
```

To work on the interface without compiling Rust at all, `npm run preview` in
`ui/` builds a single self-contained HTML file against mock data:

```sh
cd ui && npm run preview && open dist-mock/preview.html
```

## Ground rules

These are the constraints that make ccmon what it is. A change that breaks one
of them needs a very good argument.

- **No network calls, ever.** No telemetry, no update checks, no LLM. The
  report is generated mechanically so the tool is free to run and works
  offline. CI would rather fail than gain an HTTP client.
- **Read-only with respect to Claude Code's data.** ccmon writes only to its own
  data directory. The one planned exception is `ccmon install`, which backs up
  `settings.json` before touching it.
- **Every input is untrusted.** Claude Code's on-disk formats are not documented
  contracts and have already changed under this tool twice. Parse permissively:
  unknown fields kept, unknown event types stored and ignored, a bad line
  skipped rather than fatal. Never let a value from disk reach a shell without
  validation.
- **`events` is the source of truth.** Every other table is derived and must be
  reproducible by `ccmon reindex --force`. Do not put anything in a derived
  table that cannot be re-derived.
- **The hook must never block.** It reads stdin, appends one line, and exits.
  No SQLite, no network, no git, always exit 0. Latency there is latency on
  every tool call in every session the user runs.
- **Terseness in output is a feature.** The report competes for space in a chat
  context window. If you are adding a line to it, be able to say what decision
  that line changes.

## Style

- Comments explain *why*, not *what*. If a decision is non-obvious or was
  reached the hard way, say so — several comments in this codebase exist
  because the obvious approach was wrong against real data.
- Test names are sentences describing the behaviour being protected
  (`overlapping_sessions_are_attributed_to_both_as_window`), not
  `test_attribute_2`.
- New behaviour needs a test. Bug fixes need a test that fails without the fix.
- `cargo fmt` and clippy with `-D warnings` both gate CI.

## Never commit

- Real transcript content, real prompts, real client or project names. Test and
  mock data is **invented**. Transcripts are plaintext records of everything
  someone typed; treat anything derived from them as sensitive.
- Anything from your own `~/.claude`.
- Screenshots containing real session titles.

## Scope

Deliberately excluded. Each was considered and rejected, so a PR adding one
will likely be declined however good it is:

issue-tracker integration · a daemon · multi-machine sync · team features ·
mobile · code signing · LLM calls of any kind · telemetry · answering
permission prompts from the GUI (not possible).

If you want one of these, a fork is a completely reasonable answer.

## Reporting bugs

Include your OS, `ccmon --version`, and the output of `ccmon doctor` — it prints
the paths and retention settings that explain most reports. **Redact it first
if it names anything you would rather not publish.**

For anything security-relevant, see [SECURITY.md](SECURITY.md) instead of the
issue tracker.
