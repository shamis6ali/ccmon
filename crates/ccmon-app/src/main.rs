//! ccmon's tray application.
//!
//! Tray-first by design: the problem being solved is window sprawl, so the app
//! must not add another window the user has to manage. It launches minimised,
//! takes no Dock slot on macOS, and opens a real window only when asked.
//!
//! While this app runs it acts as the live updater — it watches Claude Code's
//! runtime files and ccmon's spool and re-ingests on change. When it is not
//! running, the CLI still works standalone off the same files. No third process
//! ever exists.

// Release builds on Windows must not pop a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod notify;
mod platform;
mod state;
mod tray;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use notify_rs::{RecursiveMode, Watcher};
use tauri::{Emitter, Manager};

use crate::state::AppState;

// `notify` is also the name of our own module.
use ::notify as notify_rs;

/// Coalesce bursts of filesystem events; Claude Code rewrites its runtime file
/// on every status change and a busy session touches it constantly.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Fallback poll. Liveness and staleness are time-dependent, so the UI must
/// refresh even when nothing on disk changes.
const TICK: Duration = Duration::from_secs(15);

/// Event the window listens for.
const CHANGED: &str = "ccmon://sessions-changed";

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(if cfg!(debug_assertions) {
            tracing::Level::DEBUG
        } else {
            tracing::Level::WARN
        })
        .with_target(false)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            commands::list_sessions,
            commands::refresh_now,
            commands::work_report,
            commands::copy_text,
            commands::open_path,
            commands::resume_session,
            commands::get_settings,
            commands::set_settings,
            commands::data_dir,
            commands::session_detail,
            commands::todos_for,
        ])
        .setup(|app| {
            // No Dock icon: this is a tray app, and taking a Dock slot would be
            // one more window-manager entry for a user who already has too many.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let state = AppState::new()?;
            // Populate before the tray is built so the first menu is real.
            if let Err(e) = state.refresh() {
                tracing::warn!(error = %e, "initial refresh failed");
            }
            app.manage(state);

            tray::build(app.handle())?;
            tray::sync(app.handle(), &app.state::<AppState>());

            spawn_watcher(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it. Quitting is an explicit tray action;
            // otherwise a stray Cmd-W would silently stop the monitoring.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                commands::hide_window(window.app_handle());
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start ccmon");
}

/// Watch the files that change when a session's state changes, and tick.
fn spawn_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel();

        let handler = move |res: notify_rs::Result<notify_rs::Event>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        };
        let mut watcher = match notify_rs::recommended_watcher(handler) {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!(error = %e, "file watching unavailable; falling back to polling");
                None
            }
        };

        if let Some(w) = watcher.as_mut() {
            for dir in watch_targets(&app) {
                match w.watch(&dir, RecursiveMode::NonRecursive) {
                    Ok(()) => tracing::debug!(path = %dir.display(), "watching"),
                    Err(e) => tracing::debug!(path = %dir.display(), error = %e, "cannot watch"),
                }
            }
        }

        loop {
            // Wake on a change or on the tick, whichever comes first.
            let woke_on_change = rx.recv_timeout(TICK).is_ok();
            if woke_on_change {
                // Drain the burst.
                std::thread::sleep(DEBOUNCE);
                while rx.try_recv().is_ok() {}
            }

            let state = app.state::<AppState>();
            match state.refresh() {
                Ok(blocked) => {
                    tray::sync(&app, &state);
                    notify::fire(&app, &state, &blocked);
                    let _ = app.emit(CHANGED, ());
                }
                Err(e) => tracing::warn!(error = %e, "refresh failed"),
            }
        }
    });
}

/// Directories whose contents change when a session's state changes.
///
/// `sessions/` is the important one: Claude Code rewrites `<pid>.json` on every
/// status change, which is what makes NEEDS_ACTION appear within a second
/// without any hooks installed.
fn watch_targets(_app: &tauri::AppHandle) -> Vec<PathBuf> {
    let cfg = ccmon_core::Config::load().unwrap_or_default();
    let mut dirs = Vec::new();

    for root in cfg.all_roots().found {
        for sub in ["sessions", "projects", "tasks"] {
            let dir = root.path.join(sub);
            if dir.is_dir() {
                dirs.push(dir);
            }
        }
    }
    // The spool, once hooks are installed.
    if let Ok(data) = ccmon_core::paths::data_dir() {
        dirs.push(data);
    }
    dirs
}
