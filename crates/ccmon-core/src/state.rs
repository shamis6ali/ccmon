//! State derivation. Evaluated in order; first match wins.
//!
//! | # | State          | Condition |
//! |---|----------------|-----------|
//! | 1 | `ENDED`        | `SessionEnd` seen. Terminal. |
//! | 2 | `DEAD`         | No `SessionEnd`, pid not alive. |
//! | 3 | `NEEDS_ACTION` | Waiting on the user, or the last turn died. |
//! | 4 | `WORKING`      | A turn is open and recent. |
//! | 5 | `NEEDS_REVIEW` | Turn closed, alive, dirty worktree or open tasks. |
//! | 6 | `IDLE`         | Turn closed, alive, clean, nothing pending. |
//!
//! Staleness is a **flag**, not a state: a stale NEEDS_REVIEW (finished work
//! nobody looked at) and a stale DEAD (crashed and forgotten) are different
//! problems, and collapsing them loses the information needed to act.
//!
//! Nothing here is stored. State is a pure function of the rollup plus live
//! liveness and git, computed at read time, which is what lets ccmon work with
//! no daemon sweeping timers.

use chrono::{DateTime, Duration, Utc};

use crate::config::Config;
use crate::model::{ActionKind, Liveness, Session, SessionState};

/// Everything read-time that the pure state function needs.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub session: &'a Session,
    pub now: DateTime<Utc>,
    pub liveness: Liveness,
    /// `None` when the project is not a repo or git could not be reached.
    pub worktree_dirty: Option<bool>,
    pub open_todos: i64,
    pub cfg: &'a Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Derived {
    pub state: SessionState,
    pub stale: bool,
    pub action_kind: Option<ActionKind>,
}

pub fn derive(i: Inputs<'_>) -> Derived {
    let s = i.session;
    let stale = s
        .last_event_at
        .map(|t| i.now - t > Duration::days(i.cfg.stale_after_days))
        .unwrap_or(false);

    let finish = |state, action_kind| Derived {
        state,
        stale,
        action_kind,
    };

    // 1. Cleanly ended. Terminal.
    if s.ended_at.is_some() {
        return finish(SessionState::Ended, None);
    }

    // 2. The process is gone and never said goodbye.
    if i.liveness == Liveness::Dead {
        return finish(SessionState::Dead, None);
    }

    // 3. Waiting on a human.
    //
    // Two independent sources agree on this: Claude Code's own runtime file
    // (`status: waiting`), and the Notification / StopFailure hook events. The
    // runtime file wins when present because it reflects *now* rather than the
    // last thing that happened to be logged.
    if let Some(kind) = needs_action_kind(s) {
        return finish(SessionState::NeedsAction, Some(kind));
    }

    let turn_open = match (s.last_prompt_at, s.last_stop_at) {
        (Some(p), Some(stop)) => p > stop,
        (Some(_), None) => true,
        _ => false,
    };
    let runtime_busy = s.runtime_status.as_deref() == Some("busy");

    // 4. A turn is open.
    if turn_open || runtime_busy {
        let recent = s
            .last_event_at
            .map(|t| i.now - t <= Duration::seconds(i.cfg.active_window_secs))
            .unwrap_or(false);
        if recent || runtime_busy {
            return finish(SessionState::Working, None);
        }
        // An open turn that stopped emitting: Claude Code died mid-turn
        // without the pid going away, or is stuck on a network call. That
        // wants a human, so it is NEEDS_ACTION rather than a silent WORKING.
        return finish(SessionState::NeedsAction, Some(ActionKind::StalledTurn));
    }

    // 5. Turn closed with something left on the table.
    if i.worktree_dirty == Some(true) || i.open_todos > 0 {
        return finish(SessionState::NeedsReview, None);
    }

    // 6. Nothing outstanding.
    finish(SessionState::Idle, None)
}

fn needs_action_kind(s: &Session) -> Option<ActionKind> {
    if s.runtime_status.as_deref() == Some("waiting") {
        let reason = s.waiting_for.as_deref().unwrap_or("");
        return Some(if reason.contains("permission") {
            ActionKind::PermissionPrompt
        } else {
            ActionKind::IdlePrompt
        });
    }

    match s.last_event_type.as_deref() {
        Some("StopFailure") => return Some(ActionKind::StopFailure),
        Some("Notification") => match s.last_notif_kind.as_deref() {
            Some("permission_prompt") => return Some(ActionKind::PermissionPrompt),
            Some("idle_prompt") => return Some(ActionKind::IdlePrompt),
            // `auth_success` and friends are informational, not a summons.
            _ => {}
        },
        _ => {}
    }
    None
}

/// Whether a session belongs in the UI's **Stale** group.
///
/// Stale IDLE is just finished work, so it is deliberately not surfaced.
pub fn in_stale_group(state: SessionState, stale: bool) -> bool {
    stale
        && matches!(
            state,
            SessionState::NeedsAction | SessionState::NeedsReview | SessionState::Dead
        )
}

/// Is the process behind this session still running?
///
/// Checked at read time and never stored. A recycled pid would make a dead
/// session look alive, so when we know when the process *should* have started
/// we require the running process to match.
pub fn check_liveness(
    pid: Option<i64>,
    proc_start: Option<DateTime<Utc>>,
    last_event_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Liveness {
    let pid = match pid {
        Some(p) if p > 0 => p,
        // Sessions backfilled from transcripts alone never had a pid recorded.
        // "Unknown" is the honest answer; it must not read as DEAD.
        _ => return Liveness::Unknown,
    };

    let Ok(pid_u32) = u32::try_from(pid) else {
        return Liveness::Unknown;
    };

    let mut sys = sysinfo::System::new();
    let sys_pid = sysinfo::Pid::from_u32(pid_u32);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sys_pid]), true);

    let Some(proc) = sys.process(sys_pid) else {
        return Liveness::Dead;
    };

    // PID reuse guard. `start_time` is epoch seconds.
    let actual_start = DateTime::from_timestamp(proc.start_time() as i64, 0);
    match (proc_start, actual_start) {
        (Some(expected), Some(actual)) => {
            // ctime has one-second resolution, so allow a small window.
            if (actual - expected).num_seconds().abs() <= 2 {
                Liveness::Alive
            } else {
                // Same pid, different process: the original is gone.
                Liveness::Dead
            }
        }
        _ => {
            // No start time to compare against. A pid that has been "alive"
            // for a session untouched in over a day is far more likely to be
            // recycled than genuinely still running.
            match last_event_at {
                Some(t) if now - t > Duration::hours(24) => Liveness::Dead,
                _ => Liveness::Alive,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        crate::model::parse_ts("2026-07-24T12:00:00.000Z").unwrap()
    }
    fn ago(mins: i64) -> Option<DateTime<Utc>> {
        Some(now() - Duration::minutes(mins))
    }

    fn base() -> Session {
        Session {
            session_id: "s".into(),
            project_path: "/p".into(),
            project_slug: "-p".into(),
            last_event_at: ago(1),
            ..Default::default()
        }
    }

    fn derive_with(s: &Session, liveness: Liveness, dirty: Option<bool>, todos: i64) -> Derived {
        let cfg = Config::default();
        derive(Inputs {
            session: s,
            now: now(),
            liveness,
            worktree_dirty: dirty,
            open_todos: todos,
            cfg: &cfg,
        })
    }

    #[test]
    fn ended_beats_everything() {
        let mut s = base();
        s.ended_at = ago(5);
        s.runtime_status = Some("waiting".into());
        assert_eq!(
            derive_with(&s, Liveness::Alive, Some(true), 3).state,
            SessionState::Ended
        );
    }

    #[test]
    fn dead_process_beats_a_stale_waiting_flag() {
        // A leftover runtime file must not make a crashed session look like it
        // is asking a question.
        let mut s = base();
        s.runtime_status = Some("waiting".into());
        s.waiting_for = Some("permission prompt".into());
        assert_eq!(
            derive_with(&s, Liveness::Dead, None, 0).state,
            SessionState::Dead
        );
    }

    #[test]
    fn runtime_waiting_is_needs_action_with_a_reason() {
        let mut s = base();
        s.runtime_status = Some("waiting".into());
        s.waiting_for = Some("permission prompt".into());
        let d = derive_with(&s, Liveness::Alive, None, 0);
        assert_eq!(d.state, SessionState::NeedsAction);
        assert_eq!(d.action_kind, Some(ActionKind::PermissionPrompt));

        s.waiting_for = Some("something else".into());
        assert_eq!(
            derive_with(&s, Liveness::Alive, None, 0).action_kind,
            Some(ActionKind::IdlePrompt)
        );
    }

    #[test]
    fn notification_and_stop_failure_events_are_needs_action() {
        let mut s = base();
        s.last_event_type = Some("Notification".into());
        s.last_notif_kind = Some("permission_prompt".into());
        assert_eq!(
            derive_with(&s, Liveness::Unknown, None, 0).action_kind,
            Some(ActionKind::PermissionPrompt)
        );

        s.last_notif_kind = Some("idle_prompt".into());
        assert_eq!(
            derive_with(&s, Liveness::Unknown, None, 0).action_kind,
            Some(ActionKind::IdlePrompt)
        );

        // auth_success is informational, not a summons.
        s.last_notif_kind = Some("auth_success".into());
        assert_ne!(
            derive_with(&s, Liveness::Unknown, None, 0).state,
            SessionState::NeedsAction
        );

        s.last_event_type = Some("StopFailure".into());
        assert_eq!(
            derive_with(&s, Liveness::Unknown, None, 0).action_kind,
            Some(ActionKind::StopFailure)
        );
    }

    #[test]
    fn open_recent_turn_is_working() {
        let mut s = base();
        s.last_prompt_at = ago(2);
        s.last_stop_at = ago(30);
        s.last_event_at = ago(1);
        assert_eq!(
            derive_with(&s, Liveness::Alive, Some(true), 5).state,
            SessionState::Working
        );
    }

    #[test]
    fn open_but_silent_turn_is_a_stalled_turn() {
        let mut s = base();
        s.last_prompt_at = ago(60);
        s.last_stop_at = ago(90);
        s.last_event_at = ago(45); // outside the 5 minute active window
        let d = derive_with(&s, Liveness::Alive, None, 0);
        assert_eq!(d.state, SessionState::NeedsAction);
        assert_eq!(d.action_kind, Some(ActionKind::StalledTurn));
    }

    #[test]
    fn runtime_busy_is_working_even_without_turn_timestamps() {
        let mut s = base();
        s.runtime_status = Some("busy".into());
        s.last_event_at = ago(120); // outside the active window
        assert_eq!(
            derive_with(&s, Liveness::Alive, None, 0).state,
            SessionState::Working
        );
    }

    #[test]
    fn closed_turn_splits_on_dirty_worktree_and_open_todos() {
        let mut s = base();
        s.last_prompt_at = ago(30);
        s.last_stop_at = ago(10);

        assert_eq!(
            derive_with(&s, Liveness::Alive, Some(true), 0).state,
            SessionState::NeedsReview
        );
        assert_eq!(
            derive_with(&s, Liveness::Alive, Some(false), 2).state,
            SessionState::NeedsReview
        );
        assert_eq!(
            derive_with(&s, Liveness::Alive, Some(false), 0).state,
            SessionState::Idle
        );
        // Unknown git state is not evidence of pending work.
        assert_eq!(
            derive_with(&s, Liveness::Alive, None, 0).state,
            SessionState::Idle
        );
    }

    #[test]
    fn staleness_is_a_flag_orthogonal_to_state() {
        let mut s = base();
        s.last_prompt_at = Some(now() - Duration::days(9));
        s.last_stop_at = Some(now() - Duration::days(8));
        s.last_event_at = Some(now() - Duration::days(8));

        let d = derive_with(&s, Liveness::Alive, Some(true), 0);
        assert_eq!(d.state, SessionState::NeedsReview);
        assert!(d.stale);
        assert!(in_stale_group(d.state, d.stale));

        let clean = derive_with(&s, Liveness::Alive, Some(false), 0);
        assert_eq!(clean.state, SessionState::Idle);
        assert!(clean.stale);
        assert!(
            !in_stale_group(clean.state, clean.stale),
            "stale IDLE is just finished work"
        );
    }

    #[test]
    fn missing_pid_is_unknown_not_dead() {
        assert_eq!(check_liveness(None, None, ago(1), now()), Liveness::Unknown);
        assert_eq!(
            check_liveness(Some(0), None, ago(1), now()),
            Liveness::Unknown
        );
    }

    #[test]
    fn our_own_process_reads_as_alive() {
        let me = std::process::id() as i64;
        assert_eq!(
            check_liveness(Some(me), None, Some(Utc::now()), Utc::now()),
            Liveness::Alive
        );
    }

    #[test]
    fn a_wrong_start_time_reads_as_pid_reuse() {
        let me = std::process::id() as i64;
        let bogus = crate::model::parse_ts("2001-01-01T00:00:00.000Z");
        assert_eq!(
            check_liveness(Some(me), bogus, Some(Utc::now()), Utc::now()),
            Liveness::Dead
        );
    }

    #[test]
    fn an_ancient_session_on_a_live_pid_reads_as_dead() {
        let me = std::process::id() as i64;
        let long_ago = Some(Utc::now() - Duration::days(30));
        assert_eq!(
            check_liveness(Some(me), None, long_ago, Utc::now()),
            Liveness::Dead
        );
    }

    #[test]
    fn unlikely_pid_is_dead() {
        assert_eq!(
            check_liveness(Some(4_000_000), None, ago(1), now()),
            Liveness::Dead
        );
    }
}
