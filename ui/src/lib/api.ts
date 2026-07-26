// Every call goes through a Rust command. Nothing here talks to the network,
// the filesystem, or an LLM — the backend is the only thing that reads data,
// and it only ever reads.

import { invoke } from "@tauri-apps/api/core";

export type SessionState =
  | "NEEDS_ACTION"
  | "WORKING"
  | "NEEDS_REVIEW"
  | "IDLE"
  | "DEAD"
  | "ENDED";

export type ActionKind =
  | "permission_prompt"
  | "idle_prompt"
  | "stop_failure"
  | "stalled_turn";

export interface ProjectEdits {
  project_path: string;
  edits: number;
}

export interface FileEdit {
  path: string;
  edits: number;
}

export interface AttributedCommit {
  sha: string;
  subject: string;
  ts: string;
  confidence: "exact" | "window";
  project_path: string;
}

export interface SessionView {
  session_id: string;
  project_path: string;
  summary: string | null;
  session_name: string | null;
  first_prompt: string | null;
  git_branch: string | null;
  term_program: string | null;
  last_event_at: string | null;
  started_at: string | null;
  pid: number | null;
  runtime_kind: string | null;
  state: SessionState;
  stale: boolean;
  action_kind: ActionKind | null;
  liveness: "alive" | "dead" | "unknown";
  worktree_dirty: boolean | null;
  open_todos: number;
  files: FileEdit[];
  commits: AttributedCommit[];
  projects: ProjectEdits[];
}

export interface AppSettings {
  notifications_enabled: boolean;
  stale_after_days: number;
  autostart: boolean;
}

/** Ordering used everywhere: most urgent group first. */
export const STATE_ORDER: SessionState[] = [
  "NEEDS_ACTION",
  "WORKING",
  "NEEDS_REVIEW",
  "IDLE",
  "DEAD",
  "ENDED",
];

export const STATE_LABEL: Record<SessionState, string> = {
  NEEDS_ACTION: "Needs action",
  WORKING: "Working",
  NEEDS_REVIEW: "Needs review",
  IDLE: "Idle",
  DEAD: "Dead",
  ENDED: "Ended",
};

export const ACTION_LABEL: Record<ActionKind, string> = {
  permission_prompt: "permission prompt",
  idle_prompt: "waiting for you",
  stop_failure: "turn failed",
  stalled_turn: "stalled",
};

export const listSessions = () => invoke<SessionView[]>("list_sessions");

export const refreshNow = () => invoke<SessionView[]>("refresh_now");

export const workReport = (
  since: string,
  until: string | null,
  project: string | null,
) =>
  invoke<string>("work_report", {
    since,
    until: until || null,
    project: project || null,
  });

export const copyText = (text: string) => invoke<void>("copy_text", { text });

export const openPath = (path: string) => invoke<void>("open_path", { path });

export const copySessionId = (sessionId: string) =>
  invoke<void>("copy_text", { text: sessionId });

/**
 * Resume is refused by the backend while the process is alive: two Claude Code
 * processes pointed at one session file corrupt the transcript.
 */
export const resumeSession = (sessionId: string) =>
  invoke<void>("resume_session", { sessionId });

export const getSettings = () => invoke<AppSettings>("get_settings");

export const setSettings = (settings: AppSettings) =>
  invoke<void>("set_settings", { settings });

export const dataDir = () => invoke<string>("data_dir");

/** The title that matches the terminal window title. */
export function title(s: SessionView): string {
  return s.summary || s.session_name || s.session_id;
}

/** The repo the work actually happened in, not the cwd it launched from. */
export function primaryProject(s: SessionView): string {
  return s.projects[0]?.project_path ?? s.project_path;
}

export function basename(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

export function relativeTime(iso: string | null): string {
  if (!iso) return "—";
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "—";
  const secs = Math.floor((Date.now() - then) / 1000);
  if (secs < 0) return "now";
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/** Stale IDLE is just finished work, so it is deliberately not flagged. */
export function inStaleGroup(s: SessionView): boolean {
  return (
    s.stale &&
    (s.state === "NEEDS_ACTION" ||
      s.state === "NEEDS_REVIEW" ||
      s.state === "DEAD")
  );
}
