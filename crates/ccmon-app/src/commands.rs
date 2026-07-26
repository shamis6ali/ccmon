//! Commands the window calls. Everything the UI can do goes through here.

use ccmon_core::{
    config::Config,
    model::{Liveness, SessionView},
    report, store,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::state::AppState;

/// Commands return a plain string error so the UI can show it verbatim.
type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Vec<SessionView> {
    state.snapshot()
}

#[tauri::command]
pub fn refresh_now(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Vec<SessionView>> {
    let blocked = state.refresh().map_err(err)?;
    crate::tray::sync(&app, &state);
    crate::notify::fire(&app, &state, &blocked);
    Ok(state.snapshot())
}

#[tauri::command]
pub fn work_report(
    state: State<'_, AppState>,
    since: String,
    until: Option<String>,
    project: Option<String>,
) -> CmdResult<String> {
    let cfg = state.cfg.read().map(|c| c.clone()).unwrap_or_default();

    let since = report::parse_since(&since).map_err(err)?;
    let until = match until.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => report::parse_until(s).map_err(err)?,
        None => Utc::now(),
    };
    // An inverted range renders as an empty report, which reads as "you did no
    // work" rather than "those dates are backwards".
    if until < since {
        return Err(format!(
            "End date ({}) is before the start date ({}).",
            until.format("%Y-%m-%d"),
            since.format("%Y-%m-%d")
        ));
    }

    let opts = report::ReportOptions {
        since,
        until,
        project,
        include_empty: false,
        include_ended: cfg.include_ended_in_report,
    };
    let views = state.snapshot();
    Ok(report::render_markdown(&report::build(&views, &cfg, &opts)))
}

#[tauri::command]
pub fn copy_text(app: tauri::AppHandle, text: String) -> CmdResult<()> {
    crate::platform::copy_to_clipboard(&app, &text).map_err(err)
}

#[tauri::command]
pub fn open_path(path: String) -> CmdResult<()> {
    crate::platform::open_in_file_manager(&path).map_err(err)
}

/// Resume a session in a new terminal.
///
/// Refused while the process is alive: two Claude Code processes pointed at one
/// session file corrupt the transcript. That is also precisely the case where
/// the user can find the window themselves, which is why the session title is
/// shown so prominently — it matches the terminal window title.
#[tauri::command]
pub fn resume_session(state: State<'_, AppState>, session_id: String) -> CmdResult<()> {
    let views = state.snapshot();
    let view = views
        .iter()
        .find(|v| v.session.session_id == session_id)
        .ok_or_else(|| format!("no such session: {session_id}"))?;

    if view.liveness == Liveness::Alive {
        return Err(format!(
            "That session is still running{}. Find the window titled “{}” — \
             resuming a live session would corrupt its transcript.",
            view.session
                .term_program
                .as_deref()
                .map(|t| format!(" in {t}"))
                .unwrap_or_default(),
            view.session.display_title()
        ));
    }

    let cwd = view.primary_project().to_string();
    crate::platform::resume_in_terminal(&session_id, &cwd).map_err(err)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub notifications_enabled: bool,
    pub stale_after_days: i64,
    pub autostart: bool,
}

#[tauri::command]
pub fn get_settings(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<AppSettings> {
    let cfg = state.cfg.read().map(|c| c.clone()).unwrap_or_default();
    Ok(AppSettings {
        notifications_enabled: cfg.notifications_enabled,
        stale_after_days: cfg.stale_after_days,
        autostart: crate::platform::autostart_enabled(&app),
    })
}

#[tauri::command]
pub fn set_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> CmdResult<()> {
    // Patch the file in place rather than re-serialising it: config.toml ships
    // with explanatory comments and the user may have added their own.
    Config::patch(&[
        (
            "notifications_enabled",
            settings.notifications_enabled.to_string(),
        ),
        (
            "stale_after_days",
            settings.stale_after_days.clamp(1, 365).to_string(),
        ),
    ])
    .map_err(err)?;

    if let Ok(mut cfg) = state.cfg.write() {
        cfg.notifications_enabled = settings.notifications_enabled;
        cfg.stale_after_days = settings.stale_after_days.clamp(1, 365);
    }

    crate::platform::set_autostart(&app, settings.autostart).map_err(err)?;

    // Staleness is derived at read time, so the change shows up immediately.
    let _ = state.refresh();
    crate::tray::sync(&app, &state);
    Ok(())
}

#[tauri::command]
pub fn data_dir() -> CmdResult<String> {
    ccmon_core::paths::data_dir()
        .map(|p| p.display().to_string())
        .map_err(err)
}

#[tauri::command]
pub fn session_detail(
    state: State<'_, AppState>,
    session_id: String,
) -> CmdResult<Option<SessionView>> {
    Ok(state
        .snapshot()
        .into_iter()
        .find(|v| v.session.session_id == session_id))
}

#[tauri::command]
pub fn todos_for(state: State<'_, AppState>, session_id: String) -> CmdResult<Vec<String>> {
    state
        .with_conn(|conn| {
            Ok(store::todos_for(conn, &session_id)?
                .into_iter()
                .filter(|t| t.status.is_open())
                .map(|t| t.content)
                .collect())
        })
        .map_err(err)
}

/// Bring the window up and focus it.
pub fn show_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        #[cfg(target_os = "macos")]
        {
            // Accessory apps do not take focus by default; ask for it only
            // when the user explicitly opened the window.
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        }
    }
}

/// Hide the window and, on macOS, drop back out of the Dock.
pub fn hide_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
}
