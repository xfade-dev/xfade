use crate::daemon_ctl;
use crate::dto::ProxyStatus;
use crate::state::AppState;
use std::str::FromStr;
use tauri::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;
use xfade_core::models::ToolKind;

const POLL_INTERVAL_SECS: u64 = 5;

/// Build the system tray icon and start the status-refresh loop.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    TrayIconBuilder::with_id("main")
        .tooltip("Xfade")
        .icon(app.default_window_icon().unwrap().clone())
        .on_menu_event(on_menu_event)
        .build(app)?;

    // Rebuild the menu immediately, then poll every 5s (refresh run state + autostart).
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_menu(&app).await;
            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;
        }
    });
    Ok(())
}

/// Query the daemon status and autostart state, then rebuild the tray menu in place.
async fn refresh_menu(app: &AppHandle) {
    let status = daemon_ctl::status_from_config().await;
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    let Ok(menu) = build_menu(app, &status, autostart) else {
        return;
    };
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_menu(Some(menu));
    }
}

fn build_menu(app: &AppHandle, status: &ProxyStatus, autostart: bool) -> tauri::Result<Menu<Wry>> {
    let status_text = if status.running {
        match (&status.host, status.port) {
            (Some(h), Some(p)) => format!(
                "Running {h}:{p}{}",
                if status.auth_enabled { " (auth)" } else { "" }
            ),
            _ => "Running".to_string(),
        }
    } else {
        "Stopped".to_string()
    };
    let status_item = MenuItem::with_id(app, "status", status_text, false, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "Start proxy", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop proxy", true, None::<&str>)?;
    let autostart_item = CheckMenuItem::with_id(
        app,
        "autostart",
        "Launch at login",
        true,
        autostart,
        None::<&str>,
    )?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let show = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit GUI", true, None::<&str>)?;
    let provider_submenus: Vec<Submenu<Wry>> = ToolKind::ALL
        .iter()
        .map(|&t| provider_submenu(app, t.label(), t))
        .collect::<tauri::Result<_>>()?;
    let provider_refs: Vec<&dyn IsMenuItem<Wry>> = provider_submenus
        .iter()
        .map(|s| s as &dyn IsMenuItem<Wry>)
        .collect();
    let mut items: Vec<&dyn IsMenuItem<Wry>> =
        vec![&status_item, &start, &stop, &autostart_item, &sep1];
    items.extend(provider_refs);
    items.push(&sep2);
    items.push(&show);
    items.push(&quit);
    Menu::with_items(app, &items)
}

fn provider_submenu(app: &AppHandle, label: &str, tool: ToolKind) -> tauri::Result<Submenu<Wry>> {
    let state = app.state::<AppState>();
    let providers = state.core.list(Some(tool)).unwrap_or_default();
    let current = state.core.current(tool).unwrap_or(None);
    let items: Vec<MenuItem<Wry>> = providers
        .iter()
        .map(|p| {
            let mark = if current.as_ref().is_some_and(|c| c.id == p.id) {
                "✓ "
            } else {
                ""
            };
            MenuItem::with_id(
                app,
                format!("use:{}:{}", tool.as_str(), p.id),
                format!("{mark}{}", p.id),
                true,
                None::<&str>,
            )
            .unwrap()
        })
        .collect();
    if items.is_empty() {
        let empty = MenuItem::with_id(
            app,
            format!("empty:{}", tool.as_str()),
            "(no providers)",
            false,
            None::<&str>,
        )?;
        Submenu::with_items(app, label, true, &[&empty])
    } else {
        let refs: Vec<&dyn IsMenuItem<Wry>> =
            items.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect();
        Submenu::with_items(app, label, true, &refs)
    }
}

fn on_menu_event(app: &AppHandle, e: MenuEvent) {
    let id = e.id().as_ref().to_string();
    match id.as_str() {
        "show" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        "start" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let cfg = daemon_ctl::config();
                let (h, p, t) = cfg.map(|c| (c.host, c.port, c.auth_token)).unwrap_or((
                    "127.0.0.1".into(),
                    24860,
                    None,
                ));
                let _ = daemon_ctl::start(&app, h, p, t).await;
            });
        }
        "stop" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = daemon_ctl::stop(&app).await;
            });
        }
        "autostart" => {
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let autolaunch = app.autolaunch();
                let enabled = autolaunch.is_enabled().unwrap_or(false);
                let _ = if enabled {
                    autolaunch.disable()
                } else {
                    autolaunch.enable()
                };
            });
        }
        "quit" => {
            app.exit(0);
        }
        s if s.starts_with("use:") => {
            let parts: Vec<&str> = s.splitn(3, ':').collect();
            if parts.len() == 3 {
                let tool = match ToolKind::from_str(parts[1]) {
                    Ok(t) => t,
                    Err(_) => return,
                };
                let id = parts[2].to_string();
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let state = app.state::<AppState>();
                    let _ = state.core.use_provider(tool, &id);
                });
            }
        }
        _ => {}
    }
}
