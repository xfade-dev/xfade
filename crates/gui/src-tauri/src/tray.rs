use crate::state::AppState;
use agent_switch_core::models::ToolKind;
use std::str::FromStr;
use tauri::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

/// 构建系统托盘图标 + 菜单。
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    TrayIconBuilder::with_id("main")
        .tooltip("Agent Switch")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .on_menu_event(on_menu_event)
        .build(app)?;
    Ok(())
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "启动代理", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "停止代理", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 GUI", true, None::<&str>)?;
    let claude = provider_submenu(app, "Claude Code", ToolKind::ClaudeCode)?;
    let codex = provider_submenu(app, "Codex", ToolKind::Codex)?;
    let opencode = provider_submenu(app, "OpenCode", ToolKind::OpenCode)?;
    let items: Vec<&dyn IsMenuItem<Wry>> = vec![
        &show, &start, &stop, &sep1, &claude, &codex, &opencode, &sep2, &quit,
    ];
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
            "(无 provider)",
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
                let cfg = crate::daemon_ctl::config();
                let (h, p, t) = cfg.map(|c| (c.host, c.port, c.auth_token)).unwrap_or((
                    "127.0.0.1".into(),
                    24860,
                    None,
                ));
                let _ = crate::daemon_ctl::start(&app, h, p, t).await;
            });
        }
        "stop" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::daemon_ctl::stop(&app).await;
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
