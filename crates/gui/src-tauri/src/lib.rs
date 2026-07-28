mod commands;
mod daemon_ctl;
mod dto;
mod state;

use agent_switch_core::store::secrets::{FileMockStore, KeyringStore, SecretStore};
use agent_switch_core::Core;
use state::AppState;
use std::sync::Arc;

/// 环境契约（与 CLI 一致）：
/// - `HOME`：工具配置根。
/// - `ASW_DATA_DIR`：自身数据目录（db/backups），缺省 `$HOME/.config/agent-switch`。
/// - `ASW_MOCK_SECRETS=1`：用文件 MockStore 替代系统钥匙串（仅测试/冒烟）。
fn build_core() -> Result<Core, String> {
    let use_mock = std::env::var("ASW_MOCK_SECRETS").ok().as_deref() == Some("1");
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let data = std::env::var_os("ASW_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(&home)
                .join(".config")
                .join("agent-switch")
        });
    let secrets: Arc<dyn SecretStore> = if use_mock {
        Arc::new(FileMockStore::new(data.join("mock-secrets.json")))
    } else {
        Arc::new(KeyringStore::new())
    };
    Core::with_paths(std::path::Path::new(&home), &data, secrets).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let core = build_core().expect("failed to init core");
    tauri::Builder::default()
        .manage(AppState::new(core))
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
            commands::proxy_start,
            commands::proxy_stop,
            commands::proxy_status,
            commands::get_routes,
            commands::set_routes,
            commands::clear_routes,
            commands::stats,
            commands::recent_logs,
        ])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
