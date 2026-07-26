// Mock backend for design iteration. Used only by `npm run preview`, which
// builds a single self-contained HTML file so the interface can be reviewed
// without compiling the Rust side.
//
// The fixtures are invented. They exercise the states that are awkward to
// stage on demand — a session blocked on a permission prompt, a stalled turn,
// an untitled session, a background session — because those are exactly the
// rows whose layout is easiest to get wrong and hardest to catch by accident.

const now = Date.now();
const ago = (mins: number) => new Date(now - mins * 60_000).toISOString();

const SESSIONS = [
  {
    session_id: "3f2b91c4-11a8-4d0e-9c77-6b21d5a4e880",
    project_path: "/home/dev",
    summary: "Add checkout flow and wire up payments",
    session_name: null,
    first_prompt:
      "The storefront needs a real checkout. Wire up the payment provider, add the confirmation screen, and make sure the cart survives a page reload.",
    git_branch: "feat/checkout",
    term_program: "iTerm.app",
    last_event_at: ago(1_440),
    started_at: ago(20_000),
    pid: 4321,
    runtime_kind: "interactive",
    state: "NEEDS_ACTION",
    stale: false,
    action_kind: "permission_prompt",
    liveness: "alive",
    worktree_dirty: false,
    open_todos: 2,
    files: [],
    commits: [],
    projects: [{ project_path: "/home/dev/src/storefront", edits: 32 }],
  },
  {
    // No title yet, so the row falls back to the session id: a long,
    // unbreakable string that must not blow out the layout.
    session_id: "9e738571-a7f6-468e-b89a-1d78eca918bb",
    project_path: "/home/dev",
    summary: null,
    session_name: null,
    first_prompt: null,
    git_branch: null,
    term_program: null,
    last_event_at: ago(14_400),
    started_at: ago(30_000),
    pid: 999,
    runtime_kind: "interactive",
    state: "NEEDS_ACTION",
    stale: true,
    action_kind: "stalled_turn",
    liveness: "unknown",
    worktree_dirty: null,
    open_todos: 0,
    files: [],
    commits: [],
    projects: [],
  },
  {
    session_id: "b71c0d55-3e94-42aa-8f10-2c6e9b4d7a13",
    project_path: "/home/dev/src/ccmon",
    summary: "Rewrite the ingest pipeline",
    session_name: null,
    first_prompt:
      "Split ingest into phases so a slow git repo cannot stall the whole run.",
    git_branch: "main",
    term_program: "iTerm.app",
    last_event_at: ago(0.05),
    started_at: ago(240),
    pid: 30109,
    runtime_kind: "interactive",
    state: "WORKING",
    stale: false,
    action_kind: null,
    liveness: "alive",
    worktree_dirty: true,
    open_todos: 0,
    files: [],
    commits: [],
    projects: [{ project_path: "/home/dev/src/ccmon", edits: 44 }],
  },
  {
    session_id: "c02e4a17-88bd-4f6c-b3d2-70915ee6c421",
    project_path: "/home/dev",
    summary: "Draft the release notes",
    session_name: null,
    first_prompt:
      "Write release notes for 0.4 from the commits since the last tag.",
    git_branch: "main",
    term_program: "Apple_Terminal",
    last_event_at: ago(2_880),
    started_at: ago(9_000),
    pid: 7777,
    runtime_kind: "interactive",
    state: "NEEDS_REVIEW",
    stale: false,
    action_kind: null,
    liveness: "alive",
    worktree_dirty: true,
    open_todos: 1,
    files: [],
    commits: [],
    projects: [{ project_path: "/home/dev/src/docs-site", edits: 12 }],
  },
  {
    session_id: "d5a83f60-2c71-4b19-9e08-4417ab2f5cc9",
    project_path: "/home/dev",
    summary: "Migrate the job queue off cron",
    session_name: null,
    first_prompt:
      "Replace the cron-driven jobs with a proper queue. Keep the existing retry semantics and add a dead-letter path.",
    git_branch: "main",
    term_program: "iTerm.app",
    last_event_at: ago(1_500),
    started_at: ago(21_000),
    pid: null,
    runtime_kind: "interactive",
    state: "IDLE",
    stale: false,
    action_kind: null,
    liveness: "unknown",
    worktree_dirty: false,
    open_todos: 0,
    files: [],
    commits: [],
    projects: [{ project_path: "/home/dev/src/worker", edits: 73 }],
  },
  {
    session_id: "e9017b2d-64ff-4a53-bc86-1d3f0e77a205",
    project_path: "/home/dev",
    summary: "Nightly dependency sweep",
    session_name: null,
    first_prompt:
      "Check for outdated dependencies and open a PR for the safe bumps.",
    git_branch: "main",
    term_program: null,
    last_event_at: ago(1_600),
    started_at: ago(4_000),
    pid: null,
    runtime_kind: "bg",
    state: "IDLE",
    stale: false,
    action_kind: null,
    liveness: "unknown",
    worktree_dirty: false,
    open_todos: 0,
    files: [],
    commits: [],
    projects: [{ project_path: "/home/dev/src/infra", edits: 8 }],
  },
];

const REPORT = `# Work report · 2026-07-20 → 2026-07-25
3 projects · 4 sessions · 7 commits

## storefront
\`~/src/storefront\` · branch \`feat/checkout\` · worktree clean · 2 pending todos

### Add checkout flow and wire up payments
asked: "The storefront needs a real checkout. Wire up the payment provider, add the confirmation screen, and make sure the cart survives a page reload."
2026-07-21 09:14 → 2026-07-24 16:02 · 4 commits · 32 files
- \`a3f21e9\` checkout: collect shipping address before payment
- \`8b02c14\` checkout: persist the cart across reloads
- \`1d9e007\` checkout: confirmation screen and receipt email
- \`c50288e\` fix: do not double-charge on a retried submit
files: src/checkout/Cart.tsx, src/checkout/Confirm.tsx, src/lib/cart.ts, src/api/orders.ts (+28 more)
tickets: SHOP-214

## worker
\`~/src/worker\` · branch \`main\` · worktree clean · 0 pending todos

### Migrate the job queue off cron
asked: "Replace the cron-driven jobs with a proper queue. Keep the existing retry semantics and add a dead-letter path."
2026-07-22 11:40 → 2026-07-24 12:55 · 2 commits · 73 files
- \`65aaa74\` queue: replace cron entrypoints with a worker pool _(window)_
- \`9dbde71\` queue: dead-letter path and retry backoff _(window)_
files: src/queue/pool.rs, src/queue/retry.rs, src/bin/worker.rs (+70 more)

## docs-site
\`~/src/docs-site\` · branch \`main\` · worktree dirty · 1 pending todo

### Draft the release notes
asked: "Write release notes for 0.4 from the commits since the last tag."
2026-07-23 09:12 → 2026-07-23 18:40 · 0 commits · 12 files
no commits · 1 pending todo
files: content/releases/0.4.md, src/pages/changelog.astro (+10 more)
`;

export function invoke<T>(cmd: string, _args?: unknown): Promise<T> {
  switch (cmd) {
    case "list_sessions":
    case "refresh_now":
      return Promise.resolve(SESSIONS as T);
    case "work_report":
      return Promise.resolve(REPORT as T);
    case "get_settings":
      return Promise.resolve({
        notifications_enabled: true,
        stale_after_days: 3,
        autostart: false,
      } as T);
    case "data_dir":
      return Promise.resolve("/home/dev/.local/share/ccmon" as T);
    default:
      return Promise.resolve(undefined as T);
  }
}
