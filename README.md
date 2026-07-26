# ccmon

A monitor for Claude Code CLI sessions.

If you run a lot of Claude Code sessions across many projects and terminal
windows, you cannot tell at a glance which ones are working, which are blocked
on a permission prompt, which finished and are sitting unreviewed with
uncommitted changes, which have been abandoned for days, or what actually got
done this week.

`ccmon` reads Claude Code's own on-disk artifacts, derives a state per session,
and answers those questions.

It makes **no network calls**, never invokes an LLM, and is read-only with
respect to Claude Code's data.

---

## The two jobs

**Live triage.** `ccmon ls` groups every session by state, most urgent first.

```
NEEDS_ACTION  (2)
  ! Add checkout flow and wire up payments                   1d ago  [permission_prompt, 2 todo]
    storefront · feat/checkout · iTerm.app
  ! Migrate the job queue off cron                          10d ago  [stalled_turn, stale]
    worker · main

WORKING  (1)
  * Rewrite the ingest pipeline                              2s ago
    ccmon · main

NEEDS_REVIEW  (1)
  + Draft the release notes                                  2d ago  [dirty]
    docs-site · main
```

**Weekly report.** `ccmon report --since=monday` emits markdown summarising what
shipped, per project, per session, with commits. Paste it into a chat and close
your tickets conversationally.

```markdown
# Work report · 2026-07-20 → 2026-07-25
2 projects · 3 sessions · 6 commits

## storefront
`~/src/storefront` · branch `feat/checkout` · worktree clean · 2 pending todos

### Add checkout flow and wire up payments
asked: "The storefront needs a real checkout. Wire up the payment provider, add the confirmation screen, and make sure the cart survives a page reload."
2026-07-21 09:14 → 2026-07-24 16:02 · 4 commits · 32 files
- `a3f21e9` checkout: collect shipping address before payment
- `8b02c14` checkout: persist the cart across reloads
- `1d9e007` checkout: confirmation screen and receipt email
- `c50288e` fix: do not double-charge on a retried submit
files: src/checkout/Cart.tsx, src/lib/cart.ts, src/api/orders.ts (+29 more)
tickets: SHOP-214
```

**ccmon never talks to your issue tracker.** It emits a report; you close the
tickets. That keeps it dependency-free and offline.

---

## Install

There are no prebuilt binaries yet, so this is a build from source.

**The CLI is the whole tool.** It gives you live triage and the work report on
its own. The desktop app is optional and needs more toolchain, so install the
CLI first and stop there if that is all you want.

### 1. Prerequisites

Rust 1.77.2 or newer, on every platform:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then whatever your OS needs to link a binary:

<details>
<summary><b>macOS</b></summary>

```sh
xcode-select --install     # Apple's linker and headers
```
</details>

<details>
<summary><b>Linux (Debian/Ubuntu)</b></summary>

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev git
```

For `ccmon report --copy` you also need a clipboard tool — `wl-clipboard`,
`xclip`, or `xsel`. Without one, `--copy` reports which commands it tried;
everything else works.
</details>

<details>
<summary><b>Windows</b></summary>

Install the **Visual Studio Build Tools** with the "Desktop development with
C++" workload — rustup will prompt you if they are missing. Git for Windows
provides `git`, which ccmon shells out to.
</details>

### 2. Build and install the CLI

```sh
git clone https://github.com/shamis6ali/ccmon
cd ccmon
cargo install --path crates/ccmon-cli --locked
```

That puts `ccmon` in `~/.cargo/bin`, which rustup adds to your `PATH`. Open a
new shell, then:

```sh
ccmon --version
ccmon doctor      # what it found, and whether your transcripts are being deleted
ccmon reindex     # build the index from transcripts already on disk
ccmon ls          # live triage
ccmon report      # this week's work
```

`ccmon doctor` is the right first command: it prints every path ccmon
discovered and warns if Claude Code's cleanup is about to eat your history.

> Prefer not to install onto your `PATH`? `cargo build --release` leaves the
> binary at `target/release/ccmon` and you can run it from there.

### 3. Build the desktop app (optional)

The app needs **Node 18+** on top of everything above. Install it *first* — the
build is a long one and failing at the frontend step after several minutes of
Rust compilation is a miserable way to find out.

<details>
<summary><b>macOS</b></summary>

```sh
brew install node
```

Or download an installer from [nodejs.org](https://nodejs.org). There are no
other extra packages on macOS.
</details>

<details>
<summary><b>Linux (Debian/Ubuntu)</b></summary>

The app links GTK and WebKit, which the CLI does not:

```sh
sudo apt-get install -y nodejs npm libwebkit2gtk-4.1-dev \
                        libappindicator3-dev librsvg2-dev patchelf
```

If your distro's `nodejs` is older than 18, use [nodesource](https://github.com/nodesource/distributions)
or [nvm](https://github.com/nvm-sh/nvm) instead.
</details>

<details>
<summary><b>Windows</b></summary>

Install Node from [nodejs.org](https://nodejs.org). WebView2 ships with Windows
11 and current Windows 10; on anything older, install the
[Evergreen Runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
</details>

Check both are present before starting — if either prints nothing, go back:

```sh
node --version    # v18 or newer
npm --version
```

Then, **from the repository root**:

```sh
cargo install tauri-cli --version "^2" --locked   # provides `cargo tauri`
npm --prefix ui install
cargo tauri build --bundles app
```

The bundle lands in `target/release/bundle/` — on macOS,
`bundle/macos/ccmon.app`.

For a dev build with hot reload, run `cargo tauri dev` from the repository root.

> Every command here runs from the repository root. `cargo tauri` locates the
> app by searching subfolders for `tauri.conf.json`, so there is no need to
> `cd` into the crate — and if you do end up somewhere else, it fails with
> *"Couldn't recognize the current folder as a Tauri project"*.

#### First launch

The app is **unsigned** — no code-signing certificate, no notarisation — so
your OS will object the first time:

- **macOS**: right-click the `.app` → Open → Open. Or
  `xattr -d com.apple.quarantine /path/to/ccmon.app`.
- **Windows**: SmartScreen → More info → Run anyway.
- **Linux**: `chmod +x` the AppImage.

It launches **straight to the tray with no window and no Dock icon** — that is
deliberate, since the problem being solved is window sprawl. Click the tray
icon and choose **Open ccmon** to get the window.

### Working on the UI

The interface can be built and reviewed without compiling the Rust side at all.
`npm run preview` swaps the Tauri bridge for fixtures and emits a single
self-contained HTML file:

```sh
cd ui && npm run preview && open dist-mock/preview.html
```

Design notes live at the top of `ui/src/app.css`.

### What gets installed where

Nothing runs in the background and no daemon is installed. ccmon writes only to
its own directory — `ccmon doctor` prints the path:

| Platform | Data directory |
|---|---|
| macOS | `~/Library/Application Support/ccmon/` |
| Linux | `$XDG_DATA_HOME/ccmon/` or `~/.local/share/ccmon/` |
| Windows | `%LOCALAPPDATA%\ccmon\` |

To uninstall: `cargo uninstall ccmon-cli`, delete that directory, and delete
the app bundle. Nothing else is touched.

---

## Read this before anything else: your transcripts are being deleted

Claude Code stores transcripts in plaintext under `~/.claude/projects/` and
retains them for **30 days by default**, controlled by `cleanupPeriodDays` in
`settings.json`. The cleanup runs on **every Claude Code startup** and
permanently unlinks anything older. There is no trash and no recovery.

```sh
ccmon doctor    # reports your effective retention and your oldest transcript
ccmon backup    # archives everything somewhere cleanup cannot reach
```

To keep more history, set a large finite number in `~/.claude/settings.json`:

```json
{ "cleanupPeriodDays": 365 }
```

**Do not set `cleanupPeriodDays: 0`.** Despite documentation implying it
disables cleanup, it has been reported to disable transcript persistence
entirely — you get no transcripts at all.

Deletion has also been reported to key off file mtime rather than real last
activity, so raising the setting is not a complete guarantee. Archives made by
`ccmon backup` are a first-class ingest source: add one to `archive_roots` in
`config.toml` and historical reports keep working after Claude Code has pruned
the originals.

---

## Your prompts are reproduced verbatim

The report quotes each session's first prompt and is designed to be pasted into
a chat. Transcripts are plaintext records of everything you typed, including
anything you pasted — which routinely means API keys.

ccmon masks values that are unambiguously credentials (`sk-…`, `ghp_…`,
`AKIA…`, JWTs, private key headers, `api_key = …`) before they reach report
output. This is conservative pattern matching, **not a guarantee**: it will not
catch a secret that does not look like one. Read what you paste.

Turn it off with `redact_secrets = false` if you would rather see raw output.
`ccmon.db` itself stores prompts unredacted and unencrypted — see
[SECURITY.md](SECURITY.md).

---

## Commands

| Command | What it does |
|---|---|
| `ccmon ls` | Sessions grouped by state, most urgent first. `--all`, `--state`, `--project`, `--format json` |
| `ccmon report` | Work report. `--since`, `--until`, `--project`, `--format`, `--copy`, `--include-empty` |
| `ccmon reindex` | Re-run ingest. `--force` rebuilds every derived table from scratch |
| `ccmon doctor` | Retention, discovery, and database health |
| `ccmon backup` | Archive the Claude roots. `--dest` |

`--since` accepts `monday` (default), `today`, `yesterday`, `week`, `7d`, or
`2026-07-01`. Relative forms resolve against your local time.

`--until` takes the same vocabulary plus `now` (the default), so the window can
be closed at both ends for writing up a period you have already moved past. A
bare date means the *end* of that day, so this covers all of June rather than
stopping at midnight on the 30th:

```sh
ccmon report --since=2026-06-01 --until=2026-06-30
```

Reports are built on demand, never on a schedule. Every invocation re-ingests
first, so a commit made a minute ago is in the next report. In the app, the
Report tab has the same presets plus **Custom range…** for two dates.

---

## States

Evaluated in order; first match wins.

| State | Meaning |
|---|---|
| `ENDED` | Terminated cleanly. Terminal state. |
| `DEAD` | The process is gone and never said goodbye. |
| `NEEDS_ACTION` | Waiting on a permission prompt, an idle prompt, a failed turn, or a stalled one. |
| `WORKING` | A turn is open and recent. |
| `NEEDS_REVIEW` | Turn closed, still alive, dirty worktree or open tasks. |
| `IDLE` | Turn closed, clean, nothing pending. |

**Staleness is a flag, not a state.** A stale `NEEDS_REVIEW` (finished work
nobody looked at) and a stale `DEAD` (crashed and forgotten) are different
problems, and collapsing them into one `STALE` bucket loses the information you
need to act. Stale `IDLE` is just finished work, so it is not surfaced.

---

## How it works

```
Claude Code session ──(hook)──> ccmon-hook ──append──> events.jsonl (spool)
                                                              │
  ~/.claude/projects/*.jsonl   (transcripts)                   │
  ~/.claude/sessions/*.json    (live pid + status)             │
  ~/.claude/tasks/<id>/*.json  (task lists)                    │
  git log / git status                                         │
                          └────────> ingest ────────> ccmon.db (SQLite)
                                                              │
                                    ccmon-cli · desktop app · MCP server
```

**There is no daemon.** The hook appends one NDJSON line and exits — no socket,
no port, no long-running process. Every reader drains the spool into SQLite on
demand before answering. Ingest is idempotent and tracks a byte offset, so
concurrent readers converge safely, and staleness becomes a read-time
computation rather than something a timer has to sweep.

`events` is the source of truth and is append-only. Every other table is derived
and fully rebuildable with `ccmon reindex --force`.

### Where the data comes from

Claude Code's on-disk formats are **not documented stable contracts**, so every
one of them is parsed permissively: unknown fields are kept, unknown event types
are stored and ignored, and a line that fails to parse is skipped rather than
failing the file. The formats have already changed under this tool — session
titles moved from `{"type":"summary"}` to `{"type":"ai-title"}`, and task lists
moved from `todos/` to `tasks/` — and both shapes are still accepted.

Two behaviours are worth calling out because they are not obvious:

- **A session's project is not its cwd.** Running `claude` from `$HOME` and
  editing files across several repos is normal. ccmon works out which git repos
  a session actually edited files in and groups by those, so a session that
  touched three repos appears under each with only that repo's files and
  commits. Grouping by cwd would file a week of work under a directory that
  isn't a repo and can never have commits.
- **Subagent work counts.** Transcripts nested under
  `<session-id>/subagents/**` are the session's own work and their file edits
  are attributed to the parent. Their prompts and titles are not: a subagent's
  first message is the instruction it was given, not something you typed.

### Git

Git is ground truth for what shipped. Transcripts are evidence of what was
*attempted*. Completion is never inferred from transcript content.

ccmon shells out to `git` rather than linking libgit2: it only needs `log`,
`status --porcelain`, and `rev-parse`, and shelling out respects your git config
and behaves identically to what you see in your own terminal.

Commits are attributed to sessions by time window and branch. When several
long-running sessions overlap, a commit is attributed to all of them and marked
`_(window)_` in the report so you can arbitrate — ccmon does not guess. Report
totals count unique commits, so overlapping attribution never inflates them.

### Ticket keys

Keys such as `ORCH-214` are extracted from branch names and commit subjects and
rendered in the report. This is not issue-tracker integration — no network
calls, no API, no validation that the key exists. It is just declining to throw
away an identifier you already typed.

---

## Configuration

`config.toml` in the ccmon data dir (`ccmon doctor` prints the path). Every
value is optional; deleting a line restores the default. Notable ones:

| Key | Default | Meaning |
|---|---|---|
| `stale_after_days` | `3` | Staleness threshold |
| `active_window_secs` | `300` | An open turn quieter than this is a stalled turn |
| `exclude_projects` | `[]` | Substrings; matching project paths are ignored |
| `archive_roots` | `[]` | Archived Claude roots to ingest alongside the live one |
| `only_configured_roots` | `false` | Skip auto-discovery and use `claude_roots` alone |
| `redact_secrets` | `true` | Mask credentials in report output |

---

## Status

Milestones 1 and 3 are complete:

- **M1** — core ingest, the state machine, and the CLI (`report`, `ls`,
  `reindex`, `doctor`, `backup`).
- **M3** — the desktop app: a tray icon with a live needs-action badge and a
  native menu, plus a window with Sessions, Report, and Settings. It launches
  minimised to the tray and takes no Dock slot, because the problem being
  solved is window sprawl. Closing the window hides it; quitting is an
  explicit tray action.

  The interface is an instrument panel: entirely monospace, so the vertical
  rhythm that makes a dense list scannable never breaks, with one signal colour
  per state and nothing else permitted to use those hues. Colour is always
  paired with a text label, so it never carries meaning alone. Dark by default
  with a light "paper" variant; no webfonts, because the app runs under
  `default-src 'self'` with no network access.

Still to come: hooks and `ccmon install` (M2), an MCP server (M4), and npm
distribution (M5).

M1 already delivers live triage without hooks, because Claude Code writes its
own per-process runtime files containing pid, process start time, and a live
`status` field. Hooks remain worth installing when M2 lands: those files record
*current* state only, while the spool records turn boundaries, failures, and
history that no snapshot can reconstruct.

---

## Non-goals

Deliberately excluded, not oversights: issue-tracker integration, a daemon,
multi-machine sync, team features, mobile, code signing, LLM calls of any kind,
and telemetry. ccmon also cannot answer a permission prompt for you — that
prompt lives in the session's TTY and there is no supported way to drive it from
outside.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, the ground rules that keep
ccmon what it is, and what is deliberately out of scope.

Security issues go through [private vulnerability reporting](https://github.com/shamis6ali/ccmon/security/advisories/new),
not the issue tracker. [SECURITY.md](SECURITY.md) documents the threat model
and exactly what ccmon reads and writes.

## License

Apache 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
