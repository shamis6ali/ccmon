//! Shared app state and the refresh cycle.
//!
//! The window and the tray both read the same cached snapshot, so they can
//! never disagree. Refreshing is the only thing that touches SQLite, and it is
//! serialised behind one mutex — there is still no daemon, just this process
//! doing on-demand ingest like every other ccmon front end.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use ccmon_core::{
    config::Config,
    model::{SessionState, SessionView},
    rusqlite::Connection,
    store,
};

/// Never notify about the same session more than once a minute.
const NOTIFY_DEBOUNCE: Duration = Duration::from_secs(60);

pub struct AppState {
    conn: Mutex<Connection>,
    pub cfg: RwLock<Config>,
    views: RwLock<Vec<SessionView>>,
    /// Last state seen per session, so we can notify on *transitions* rather
    /// than on every refresh.
    previous: Mutex<HashMap<String, SessionState>>,
    notified_at: Mutex<HashMap<String, Instant>>,
}

impl AppState {
    pub fn new() -> Result<Self> {
        let cfg = Config::load().unwrap_or_default();
        let conn = ccmon_core::db::open_default()?;
        Ok(Self {
            conn: Mutex::new(conn),
            cfg: RwLock::new(cfg),
            views: RwLock::new(Vec::new()),
            previous: Mutex::new(HashMap::new()),
            notified_at: Mutex::new(HashMap::new()),
        })
    }

    /// The cached snapshot. Cheap; safe to call from any command.
    pub fn snapshot(&self) -> Vec<SessionView> {
        self.views.read().map(|v| v.clone()).unwrap_or_default()
    }

    /// Re-ingest and rebuild the snapshot.
    ///
    /// Returns the sessions that just *entered* NEEDS_ACTION and are not
    /// inside the notification debounce window.
    pub fn refresh(&self) -> Result<Vec<SessionView>> {
        let cfg = self.cfg.read().map(|c| c.clone()).unwrap_or_default();

        let fresh = {
            let conn = self
                .conn
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            ccmon_core::ingest::run(&conn, &cfg)?;
            store::build_views(&conn, &cfg, chrono::Utc::now())?
        };

        let newly_blocked = self.diff_for_notifications(&fresh);

        if let Ok(mut views) = self.views.write() {
            *views = fresh;
        }
        Ok(newly_blocked)
    }

    /// Which sessions crossed *into* NEEDS_ACTION since the last refresh.
    ///
    /// Only this transition is worth interrupting the user for. Notifying on
    /// NEEDS_REVIEW or IDLE with this many concurrent sessions would be
    /// constant noise, and the tray badge already carries those.
    fn diff_for_notifications(&self, fresh: &[SessionView]) -> Vec<SessionView> {
        let Ok(mut previous) = self.previous.lock() else {
            return Vec::new();
        };
        let Ok(mut notified) = self.notified_at.lock() else {
            return Vec::new();
        };

        let first_run = previous.is_empty();
        let mut out = Vec::new();

        for view in fresh {
            let id = view.session.session_id.clone();
            let was = previous.insert(id.clone(), view.state);

            if view.state != SessionState::NeedsAction {
                continue;
            }
            // On the very first refresh everything looks like a transition;
            // do not open the app with a burst of notifications.
            if first_run || was == Some(SessionState::NeedsAction) {
                continue;
            }
            let recent = notified
                .get(&id)
                .is_some_and(|t| t.elapsed() < NOTIFY_DEBOUNCE);
            if recent {
                continue;
            }
            notified.insert(id, Instant::now());
            out.push(view.clone());
        }

        // Forget sessions that disappeared so a later reappearance notifies.
        let live: std::collections::HashSet<&str> = fresh
            .iter()
            .map(|v| v.session.session_id.as_str())
            .collect();
        previous.retain(|id, _| live.contains(id.as_str()));

        out
    }

    /// Run a closure with the database, for commands that need a query.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        f(&conn)
    }
}
