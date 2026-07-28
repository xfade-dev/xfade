use crate::dto::{AddProviderInput, PresetDto, ProxyStatus, RoutesDto, UpdateProviderInput};
use crate::state::AppState;
use agent_switch_core::models::{Provider, ToolKind};
use agent_switch_core::presets::presets_for;
use agent_switch_core::store::db::{RequestLog, StatsGroupBy, StatsRow};
use tauri::State;

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ----- provider 管理 -----

#[tauri::command]
pub async fn list_providers(
    state: State<'_, AppState>,
    tool: Option<ToolKind>,
) -> CmdResult<Vec<Provider>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.list(tool))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn add_provider(state: State<'_, AppState>, input: AddProviderInput) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut p = Provider::new(&input.id, input.tool, input.base_url);
        p.extra = input.extra;
        core.add_provider(p, input.key.as_deref())
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
pub async fn update_provider(
    state: State<'_, AppState>,
    input: UpdateProviderInput,
) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut p = core
            .list(Some(input.tool))?
            .into_iter()
            .find(|p| p.id == input.id)
            .ok_or_else(|| {
                agent_switch_core::CoreError::ProviderNotFound(format!(
                    "{}/{}",
                    input.tool, input.id
                ))
            })?;
        if let Some(u) = input.base_url {
            p.base_url = Some(u);
        }
        if !input.extra.is_null() {
            p.extra = input.extra;
        }
        core.update_provider(&p, input.key.as_deref())
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
pub async fn remove_provider(
    state: State<'_, AppState>,
    tool: ToolKind,
    id: String,
) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.remove(tool, &id))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn use_provider(state: State<'_, AppState>, tool: ToolKind, id: String) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.use_provider(tool, &id))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn current_provider(
    state: State<'_, AppState>,
    tool: ToolKind,
) -> CmdResult<Option<Provider>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.current(tool))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn list_presets(tool: ToolKind) -> CmdResult<Vec<PresetDto>> {
    Ok(presets_for(tool).into_iter().map(PresetDto::from).collect())
}

#[tauri::command]
pub async fn import_provider(
    state: State<'_, AppState>,
    tool: ToolKind,
) -> CmdResult<Option<Provider>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.import(tool))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn list_backups(state: State<'_, AppState>, tool: ToolKind) -> CmdResult<Vec<String>> {
    let core = state.core.clone();
    let paths = tauri::async_runtime::spawn_blocking(move || core.backups(tool))
        .await
        .map_err(err)?
        .map_err(err)?;
    Ok(paths.into_iter().map(|p| p.display().to_string()).collect())
}

#[tauri::command]
pub async fn restore_backup(
    state: State<'_, AppState>,
    tool: ToolKind,
    path: String,
) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        core.restore_backup(tool, std::path::Path::new(&path))
    })
    .await
    .map_err(err)?
    .map_err(err)
}

// ----- 代理 / 路由 / 统计 / 日志 -----

#[tauri::command]
pub async fn proxy_start(
    app: tauri::AppHandle,
    host: String,
    port: u16,
    auth_token: Option<String>,
) -> CmdResult<ProxyStatus> {
    // 经 sidecar 执行 `asw serve install`（GUI 自装 daemon，无需 CLI 前置安装）。
    crate::daemon_ctl::start(&app, host, port, auth_token).await?;
    for _ in 0..8 {
        let s = crate::daemon_ctl::status_from_config().await;
        if s.running {
            return Ok(s);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok(crate::daemon_ctl::status_from_config().await)
}

#[tauri::command]
pub async fn proxy_stop(app: tauri::AppHandle) -> CmdResult<ProxyStatus> {
    crate::daemon_ctl::stop(&app).await?;
    Ok(crate::daemon_ctl::status_from_config().await)
}

#[tauri::command]
pub async fn proxy_status() -> CmdResult<ProxyStatus> {
    Ok(crate::daemon_ctl::status_from_config().await)
}

#[tauri::command]
pub async fn get_routes(state: State<'_, AppState>) -> CmdResult<Option<RoutesDto>> {
    let core = state.core.clone();
    let r = tauri::async_runtime::spawn_blocking(move || core.db().get_routes())
        .await
        .map_err(err)?
        .map_err(err)?;
    Ok(
        r.map(|(routes, model_override, target_protocol)| RoutesDto {
            routes,
            model_override,
            target_protocol,
        }),
    )
}

#[tauri::command]
pub async fn set_routes(
    state: State<'_, AppState>,
    routes: Vec<String>,
    model_override: Option<String>,
    target_protocol: String,
) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        core.db()
            .set_routes(&routes, model_override.as_deref(), &target_protocol)
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
pub async fn clear_routes(state: State<'_, AppState>) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().clear_routes())
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn stats(
    state: State<'_, AppState>,
    since: String,
    by: StatsGroupBy,
) -> CmdResult<Vec<StatsRow>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().stats_since(&since, by))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn recent_logs(state: State<'_, AppState>, limit: usize) -> CmdResult<Vec<RequestLog>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().recent_request_logs(limit))
        .await
        .map_err(err)?
        .map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_switch_core::models::ToolKind;

    #[test]
    fn list_presets_converts() {
        let v = presets_for(ToolKind::Codex)
            .into_iter()
            .map(PresetDto::from)
            .collect::<Vec<_>>();
        assert!(v.iter().any(|p| p.id == "official" && p.is_official));
        assert!(v.iter().any(|p| p.id == "local-proxy" && !p.is_official));
    }
}
