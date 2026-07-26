//! Native notifications.
//!
//! Only the transition *into* NEEDS_ACTION is worth interrupting for. With 15+
//! concurrent sessions, notifying on NEEDS_REVIEW or IDLE would be constant
//! noise, and the tray badge already carries those. Debouncing lives in
//! `AppState`, which only hands us sessions that genuinely just crossed over.

use ccmon_core::model::SessionView;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use crate::state::AppState;

pub fn fire(app: &AppHandle, state: &AppState, newly_blocked: &[SessionView]) {
    let enabled = state
        .cfg
        .read()
        .map(|c| c.notifications_enabled)
        .unwrap_or(true);
    if !enabled || newly_blocked.is_empty() {
        return;
    }

    // One notification per session reads as spam past a couple at once.
    if newly_blocked.len() > 2 {
        message(
            app,
            "Sessions need you",
            &format!("{} sessions are waiting for input.", newly_blocked.len()),
        );
        return;
    }

    for view in newly_blocked {
        let reason = view
            .action_kind
            .map(|k| k.as_str().replace('_', " "))
            .unwrap_or_else(|| "needs you".into());
        let where_ = view
            .session
            .term_program
            .as_deref()
            .map(|t| format!(" · {t}"))
            .unwrap_or_default();

        message(
            app,
            &view.session.display_title(),
            &format!(
                "{reason} — {}{where_}",
                ccmon_core::paths::project_name(view.primary_project())
            ),
        );
    }
}

/// Best-effort notification. A failure here must never break a refresh.
pub fn message(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::debug!(error = %e, "notification failed");
    }
}
