# Security

## Reporting a vulnerability

Report privately through GitHub's **[private vulnerability reporting](https://github.com/shamis6ali/ccmon/security/advisories/new)**
(Security → Report a vulnerability). Please do not open a public issue for
anything exploitable.

Expect an acknowledgement within a week. This is a small project maintained in
spare time — there is no paid security team and no bounty. Fixes ship in the
next release; if something is being actively exploited, say so and it gets
priority.

## What ccmon touches

Understanding the blast radius matters more than a list of promises:

- **It reads Claude Code's data.** Transcripts, per-session runtime files, and
  task lists under `~/.claude` (or wherever they live). Read-only, always.
- **It writes only its own files**, under its own data directory: a SQLite
  database, `config.toml`, an event spool, and logs.
- **It shells out to `git`** (`log`, `status --porcelain`, `rev-parse`) in
  directories that Claude Code sessions edited files in.
- **It makes no network calls.** There is no HTTP client anywhere in the CLI's
  dependency tree, no telemetry, no update check, and no LLM call. The report
  is generated mechanically.
- **It never writes to Claude Code's data.** The single planned exception is
  `ccmon install` (not yet released), which will edit `settings.json` to
  register hooks and back it up first.

## Trust boundaries

Everything ccmon reads is treated as **untrusted input**, because none of it is
a documented stable format and all of it is attacker-influenceable in principle:

- **Transcripts** are NDJSON written by another program. They are parsed
  permissively — a malformed, truncated, or hostile line is skipped, never
  fatal — and no field is used to construct a shell command.
- **Session ids** reach us from JSON fields and from directory names on disk.
  Before an id can appear in any command line it is validated against
  `[A-Za-z0-9_-]{1,64}`; anything else is refused rather than escaped.
- **Paths** are quoted for whichever shell will see them. On Windows, a path
  containing characters `cmd` cannot quote safely is handed back to the user
  instead of being guessed at.
- **SQL** is parameterised throughout. No user-derived value is formatted into
  a query string.
- **The webview** runs under `default-src 'self'` with no network access and no
  remote content. The Tauri capability set is limited to what the window
  actually uses.

## Your transcripts are sensitive

This is the most important thing on this page.

Claude Code transcripts are **plaintext records of everything you typed**,
including anything you pasted. That routinely includes API keys, tokens,
customer names, and internal system details.

Consequences worth being deliberate about:

- **`ccmon.db` inherits that sensitivity.** It stores each session's first
  prompt (up to 2000 characters) and the paths of every file edited. Protect it
  like you protect `~/.claude` itself. It is not encrypted.
- **`ccmon backup` copies transcripts verbatim.** Wherever you point it inherits
  the same sensitivity. Do not put an archive in a synced or shared folder
  without thinking about it.
- **The report is designed to be pasted elsewhere.** ccmon masks values that are
  unambiguously credentials (`sk-…`, `ghp_…`, `AKIA…`, JWTs, private key
  headers, `api_key = …`) before they reach report output. This is
  conservative pattern matching, **not a guarantee** — it will not catch a
  secret that does not look like one. Read what you paste. Redaction can be
  disabled with `redact_secrets = false`, which is a deliberate choice.

## Supply chain

`Cargo.lock` and `package-lock.json` are committed. CI runs `cargo deny` on
every push, which checks advisories, licences, and sources.

Known and accepted, with the reasoning recorded in `deny.toml`:

- **RUSTSEC-2024-0429** — unsoundness in `glib`'s `VariantStrIter`. Reached only
  through Tauri's Linux tray dependency (`tray-icon` → `libappindicator` →
  `gtk 0.18`), which pins `glib ^0.18` while the fix is in 0.20. It cannot be
  resolved downstream; it needs Tauri to migrate off the archived GTK3
  bindings. Linux-only, in an API ccmon never calls.
- Several **unmaintained** advisories for the GTK3 bindings and `unic-*`
  crates, reached the same way. `cargo deny` is configured to fail on
  unmaintained crates this workspace depends on *directly*, where something can
  actually be done, rather than on transitive dependencies with no available
  successor.

Released binaries are **unsigned**: no code-signing certificate and no
notarisation. You will need to bypass Gatekeeper or SmartScreen on first launch.
Build from source if that trade-off is not acceptable to you.

## Not vulnerabilities

- ccmon cannot answer a permission prompt for you. That prompt lives in the
  session's TTY and there is no supported way to drive it from outside.
- Resume is refused while a session's process is alive. That is deliberate: two
  Claude Code processes pointed at one session file corrupt the transcript.
- Reading another user's transcripts requires already having read access to
  their home directory, at which point ccmon is not the weak link.
