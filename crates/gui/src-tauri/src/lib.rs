mod commands;
mod daemon_ctl;
mod dto;
mod state;
mod tray;

use xfade_core::Core;
use state::AppState;
use tauri_plugin_autostart::MacosLauncher;

fn build_core() -> Result<Core, String> {
    Core::from_env().map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let core = build_core().expect("failed to init core");
    tauri::Builder::default()
        .manage(AppState::new(core))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_providers,
            commands::add_provider,
            commands::update_provider,
            commands::remove_provider,
            commands::use_provider,
            commands::current_provider,
            commands::list_presets,
            commands::import_provider,
            commands::list_backups,
            commands::restore_backup,
            commands::get_config,
            commands::set_config,
            commands::proxy_start,
            commands::proxy_stop,
            commands::proxy_status,
            commands::get_routes,
            commands::set_routes,
            commands::clear_routes,
            commands::stats,
            commands::recent_logs,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(|app| {
            tray::build(app.handle())?;
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            // Autostart is user-controlled via the tray's "Launch at login" checkbox;
            // it is NOT force-enabled here.
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
