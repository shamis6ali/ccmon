#!/usr/bin/env bash
#
# Build a throwaway Claude Code data tree so ccmon can be exercised without
# real transcripts.
#
# Transcripts are plaintext records of everything you typed, so "just copy your
# ~/.claude somewhere" is bad advice for a VM, a shared machine, or a bug
# report. This generates invented data that covers the states worth looking at:
# a session blocked on a permission prompt, one working, one finished with a
# dirty worktree, and a real git repo with real commits behind them.
#
#   ./scripts/demo-fixture.sh [target-dir]
#
# Nothing outside the target directory is touched, and your real ~/.claude is
# never read.

set -euo pipefail

ROOT="${1:-${TMPDIR:-/tmp}/ccmon-demo}"
CLAUDE="$ROOT/claude"
REPO="$ROOT/storefront"
DATA="$ROOT/data"

command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }
command -v git >/dev/null || { echo "git is required" >&2; exit 1; }

rm -rf "$ROOT"
mkdir -p "$CLAUDE/projects/-home-dev" "$CLAUDE/sessions" "$DATA"

# --- a real repo, so git collection has something true to report -----------
mkdir -p "$REPO"
git -C "$REPO" init -q -b main
git -C "$REPO" config user.email "dev@example.com"
git -C "$REPO" config user.name "Demo"
git -C "$REPO" config commit.gpgsign false

mkdir -p "$REPO/src"
printf 'export function cart() {}\n' > "$REPO/src/cart.ts"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m "SHOP-214 cart: persist across reloads"

printf 'export function checkout() {}\n' > "$REPO/src/checkout.ts"
git -C "$REPO" add -A
git -C "$REPO" commit -q -m "SHOP-214 checkout: confirmation screen"

# Leave the worktree dirty so one session lands in NEEDS_REVIEW.
printf '\n// work in progress\n' >> "$REPO/src/checkout.ts"

# --- the Claude Code tree --------------------------------------------------
# Built with python3 rather than heredocs: paths go inside JSON strings, and a
# Windows path or a directory containing a quote would produce lines no parser
# accepts. Same reason the integration tests stopped hand-writing JSON.
python3 - "$CLAUDE" "$REPO" <<'PY'
import json, os, sys, time
from datetime import datetime, timedelta, timezone

claude, repo = sys.argv[1], sys.argv[2]
now = datetime.now(timezone.utc)
iso = lambda m: (now - timedelta(minutes=m)).isoformat().replace("+00:00", "Z")
ms = lambda m: int((now - timedelta(minutes=m)).timestamp() * 1000)

SESSIONS = [
    # (id, title, prompt, files, runtime status, waitingFor, minutes ago)
    ("3f2b91c4-11a8-4d0e-9c77-6b21d5a4e880",
     "Add checkout flow and wire up payments",
     "The storefront needs a real checkout. Wire up the provider and make sure the cart survives a reload.",
     ["src/checkout.ts", "src/cart.ts"], "waiting", "permission prompt", 12),
    ("b71c0d55-3e94-42aa-8f10-2c6e9b4d7a13",
     "Rewrite the ingest pipeline",
     "Split ingest into phases so a slow repo cannot stall the whole run.",
     ["src/cart.ts"], "busy", None, 1),
    ("c02e4a17-88bd-4f6c-b3d2-70915ee6c421",
     "Draft the release notes",
     "Write release notes for 0.4 from the commits since the last tag.",
     ["src/checkout.ts"], "idle", None, 240),
]

projects = os.path.join(claude, "projects", "-home-dev")

for sid, title, prompt, files, status, waiting, mins in SESSIONS:
    lines = [
        {"type": "mode", "mode": "normal", "sessionId": sid},
        {"type": "user", "sessionId": sid, "cwd": os.path.expanduser("~"),
         "gitBranch": "main", "timestamp": iso(mins + 60),
         "message": {"role": "user", "content": prompt}},
        {"type": "ai-title", "aiTitle": title, "sessionId": sid},
    ]
    for f in files:
        lines.append({
            "type": "assistant", "sessionId": sid, "timestamp": iso(mins),
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "name": "Edit",
                "input": {"file_path": os.path.join(repo, f)},
            }]},
        })
    with open(os.path.join(projects, f"{sid}.jsonl"), "w") as fh:
        fh.write("\n".join(json.dumps(o) for o in lines) + "\n")

    # The runtime file is what makes live state work with no hooks installed.
    # pid 1 always exists, and omitting procStart makes ccmon fall back to
    # "recent activity means alive" rather than a start-time comparison.
    runtime = {
        "pid": 1, "sessionId": sid, "cwd": os.path.expanduser("~"),
        "startedAt": ms(mins + 60), "kind": "interactive", "entrypoint": "cli",
        "name": title.lower().replace(" ", "-")[:32],
        "status": status, "updatedAt": ms(mins), "statusUpdatedAt": ms(mins),
    }
    if waiting:
        runtime["waitingFor"] = waiting
    with open(os.path.join(claude, "sessions", f"{1000 + len(sid) % 100}{sid[:2]}.json"), "w") as fh:
        json.dump(runtime, fh)

# An open task, so one session has work left on the table.
tasks = os.path.join(claude, "tasks", SESSIONS[0][0])
os.makedirs(tasks, exist_ok=True)
with open(os.path.join(tasks, "1.json"), "w") as fh:
    json.dump({"id": "1", "subject": "finish the receipt email",
               "status": "pending"}, fh)

print(f"{len(SESSIONS)} sessions written")
PY

# --- point ccmon at it, and only at it -------------------------------------
cat > "$DATA/config.toml" <<EOF
# Generated by scripts/demo-fixture.sh — throwaway.
claude_roots = ["$CLAUDE"]
only_configured_roots = true
EOF

cat <<EOF

Demo fixture ready at $ROOT

Point ccmon at it for this shell only — your real data is untouched:

  export CCMON_DATA_DIR="$DATA"
  ccmon reindex
  ccmon ls
  ccmon report --since=today

Open a new shell (or unset CCMON_DATA_DIR) to go back to your real sessions.
Delete it all with:  rm -rf "$ROOT"
EOF
