//! The tray icon and its menu.
//!
//! The tray is the primary surface: the stated problem is window sprawl, so the
//! app must not add another window to manage. A real window opens only on
//! explicit action.
//!
//! The menu is built natively rather than as a webview popup. Tauri v2's tray
//! works on all three desktops, but **cursor enter/move/leave events are not
//! emitted on Linux**, so the design is click-to-open-menu only and no
//! information ever lives in a hover tooltip.

use ccmon_core::model::{SessionState, SessionView};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Manager,
};

use crate::state::AppState;

pub const TRAY_ID: &str = "main";

/// Sessions listed individually in the menu. Beyond this the menu stops being
/// scannable, which is the only thing it is for.
const MAX_LISTED: usize = 12;

/// Groups worth surfacing in the tray. Idle, dead, and ended work is not
/// triage; it lives in the window.
const TRIAGE: [SessionState; 3] = [
    SessionState::NeedsAction,
    SessionState::Working,
    SessionState::NeedsReview,
];

pub fn build(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let menu = MenuBuilder::new(app)
        .text("open", "Open ccmon")
        .separator()
        .text("quit", "Quit ccmon")
        .build()?;

    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("ccmon")
        .on_menu_event(on_menu_event)
        .build(app)
}

/// Rebuild the menu and badge from the current snapshot.
pub fn sync(app: &AppHandle, state: &AppState) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let views = state.snapshot();
    let blocked = views
        .iter()
        .filter(|v| v.state == SessionState::NeedsAction)
        .count();

    // macOS renders text beside the tray icon; that is the badge. Other
    // platforms ignore it, and the menu header carries the same count.
    #[cfg(target_os = "macos")]
    {
        let _ = tray.set_title(if blocked > 0 {
            Some(blocked.to_string())
        } else {
            None
        });
    }

    let _ = tray.set_tooltip(Some(if blocked > 0 {
        format!(
            "ccmon — {blocked} session{} waiting on you",
            if blocked == 1 { "" } else { "s" }
        )
    } else {
        "ccmon".to_string()
    }));

    if let Ok(menu) = build_menu(app, &views) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn build_menu(
    app: &AppHandle,
    views: &[SessionView],
) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let mut menu = MenuBuilder::new(app);
    let mut listed = 0usize;

    for state in TRIAGE {
        let rows: Vec<&SessionView> = views.iter().filter(|v| v.state == state).collect();
        if rows.is_empty() {
            continue;
        }

        let header =
            MenuItemBuilder::with_id(format!("hdr:{state:?}"), group_label(state, rows.len()))
                .enabled(false)
                .build(app)?;
        menu = menu.item(&header);

        for view in rows {
            if listed >= MAX_LISTED {
                break;
            }
            listed += 1;
            menu = menu.item(&session_submenu(app, view)?);
        }
        menu = menu.separator();
    }

    if listed == 0 {
        let idle = MenuItemBuilder::with_id("hdr:idle", "Nothing needs you")
            .enabled(false)
            .build(app)?;
        menu = menu.item(&idle).separator();
    }

    // The whole ticket workflow is copy-then-paste-into-chat, so the report is
    // deliberately reachable in two clicks from the tray.
    menu.text("copy-report", "Copy this week's report")
        .text("open", "Open ccmon")
        .text("refresh", "Refresh now")
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some("Quit ccmon"))?)
        .build()
}

fn group_label(state: SessionState, n: usize) -> String {
    let name = match state {
        SessionState::NeedsAction => "NEEDS ACTION",
        SessionState::Working => "WORKING",
        SessionState::NeedsReview => "NEEDS REVIEW",
        other => return format!("{} ({n})", other.as_str()),
    };
    format!("{name} ({n})")
}

fn session_submenu(
    app: &AppHandle,
    view: &SessionView,
) -> tauri::Result<tauri::menu::Submenu<tauri::Wry>> {
    let id = &view.session.session_id;
    let alive = view.liveness == ccmon_core::model::Liveness::Alive;

    // The title matches the terminal window title, which is what lets the user
    // find the right window without any OS-level window introspection.
    let label = format!(
        "{}  {}",
        marker(view),
        truncate(&view.session.display_title(), 46)
    );

    let mut sub = SubmenuBuilder::new(app, label);

    let where_ = MenuItemBuilder::with_id(format!("info:{id}"), describe(view))
        .enabled(false)
        .build(app)?;
    sub = sub.item(&where_).separator();

    // Resume is disabled while the process is alive: two Claude Code processes
    // on one session file corrupt the transcript.
    let resume = MenuItemBuilder::with_id(format!("resume:{id}"), "Resume in new terminal")
        .enabled(!alive)
        .build(app)?;
    sub = sub.item(&resume);

    sub.text(format!("folder:{id}"), "Open project folder")
        .text(format!("copyid:{id}"), "Copy session ID")
        .text(format!("show:{id}"), "Show in ccmon")
        .build()
}

fn marker(view: &SessionView) -> &'static str {
    match view.state {
        SessionState::NeedsAction => "!",
        SessionState::Working => "*",
        SessionState::NeedsReview => "+",
        SessionState::Idle => "-",
        SessionState::Dead => "x",
        SessionState::Ended => ".",
    }
}

fn describe(view: &SessionView) -> String {
    let mut parts = vec![ccmon_core::paths::project_name(view.primary_project())];
    if let Some(kind) = view.action_kind {
        parts.push(kind.as_str().replace('_', " "));
    }
    if let Some(term) = view.session.term_program.as_deref() {
        parts.push(term.to_string());
    }
    if view.worktree_dirty == Some(true) {
        parts.push("dirty".into());
    }
    if view.open_todos > 0 {
        parts.push(format!("{} todo", view.open_todos));
    }
    parts.join(" · ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().0.as_str();
    let state = app.state::<AppState>();

    match id {
        "open" => crate::commands::show_window(app),
        "quit" => app.exit(0),
        "refresh" => {
            if let Ok(blocked) = state.refresh() {
                sync(app, &state);
                crate::notify::fire(app, &state, &blocked);
            }
        }
        "copy-report" => copy_week_report(app, &state),
        _ => {
            let Some((action, session_id)) = id.split_once(':') else {
                return;
            };
            match action {
                "resume" => {
                    if let Err(e) =
                        crate::commands::resume_session(state.clone(), session_id.to_string())
                    {
                        crate::notify::message(app, "Cannot resume", &e);
                    }
                }
                "folder" => {
                    if let Some(v) = state
                        .snapshot()
                        .iter()
                        .find(|v| v.session.session_id == session_id)
                    {
                        let _ = crate::platform::open_in_file_manager(v.primary_project());
                    }
                }
                "copyid" => {
                    let _ = crate::platform::copy_to_clipboard(app, session_id);
                }
                "show" => crate::commands::show_window(app),
                _ => {}
            }
        }
    }
}

fn copy_week_report(app: &AppHandle, state: &AppState) {
    let cfg = state.cfg.read().map(|c| c.clone()).unwrap_or_default();
    let Ok(since) = ccmon_core::report::parse_since("monday") else {
        return;
    };
    let opts = ccmon_core::report::ReportOptions {
        since,
        // The tray shortcut is always "up to now" by definition.
        until: chrono::Utc::now(),
        project: None,
        include_empty: false,
        include_ended: cfg.include_ended_in_report,
    };
    let views = state.snapshot();
    let built = ccmon_core::report::build(&views, &cfg, &opts);
    let markdown = ccmon_core::report::render_markdown(&built);

    match crate::platform::copy_to_clipboard(app, &markdown) {
        Ok(()) => crate::notify::message(
            app,
            "Report copied",
            &format!(
                "{} projects · {} sessions · {} commits",
                built.projects.len(),
                built.total_sessions,
                built.total_commits
            ),
        ),
        Err(e) => crate::notify::message(app, "Could not copy report", &e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_adds_an_ellipsis_only_when_needed() {
        assert_eq!(truncate("short", 46), "short");
        let long = "x".repeat(80);
        let out = truncate(&long, 46);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 47);
    }

    #[test]
    fn group_labels_carry_the_count() {
        assert_eq!(
            group_label(SessionState::NeedsAction, 3),
            "NEEDS ACTION (3)"
        );
        assert_eq!(group_label(SessionState::Working, 1), "WORKING (1)");
    }
}
