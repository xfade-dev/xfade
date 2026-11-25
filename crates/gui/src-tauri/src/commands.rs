use crate::dto::{
    AddProviderInput, ConfigDto, ConfigInput, ModelMapDto, ModelMappingDto, PresetDto, ProxyStatus,
    RoutesDto, UpdateProviderInput,
};
use crate::state::AppState;
use std::collections::BTreeMap;
use std::time::Duration;
use tauri::State;
use xfade_core::models::{Provider, ProviderProtocol, ToolKind};
use xfade_core::presets::presets_for;
use xfade_core::security::doctor::SecurityDoctorReport;
use xfade_core::service::{probe_latency, probe_models_http, probe_target};
use xfade_core::store::db::{StatsGroupBy, StatsRow};

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Mask a secret key for display using char-safe boundaries (`••••` for `< 12` chars,
/// otherwise `first3…last4`).
fn mask_key(k: &str) -> String {
    let chars: Vec<char> = k.chars().collect();
    if chars.len() < 12 {
        return "••••".to_string();
    }
    let prefix: String = chars[..3].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    format!("{prefix}…{suffix}")
}

// ----- provider management -----

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
        let requested = input
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());
        if !input.official && requested.is_none() && core.effective_base_url(None, false).is_none()
        {
            return Err(xfade_core::CoreError::ConfigParse {
                path: "base_url".into(),
                msg: "Base URL required (set global default in Settings or enable Official)".into(),
            });
        }
        let extra = if input.extra.is_null() {
            serde_json::Value::Null
        } else {
            input.extra
        };
        core.add_provider_from_input(xfade_core::AddProviderInput {
            id: input.id.trim().to_string(),
            tool: input.tool,
            base_url: requested.map(|u| u.to_string()),
            api_key: input.key,
            protocol: input.protocol,
            extra,
            official: input.official,
            overwrite: false,
        })
        .map(|_| ())
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
                xfade_core::CoreError::ProviderNotFound(format!("{}/{}", input.tool, input.id))
            })?;
        let was_active = p.is_active;
        let direct = was_active && core.is_direct(p.tool).unwrap_or(false);
        p.base_url = input.base_url;
        p.protocol = match input.protocol {
            Some(proto) => proto,
            None => Provider::new(&p.id, p.tool, p.base_url.clone()).protocol,
        };
        p.extra = input.extra;
        core.update_provider(&p, input.key.as_deref())?;
        if was_active {
            core.use_provider(p.tool, &p.id, direct)?;
        }
        Ok(())
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
pub async fn use_provider(
    state: State<'_, AppState>,
    tool: ToolKind,
    id: String,
    direct: Option<bool>,
) -> CmdResult<()> {
    let core = state.core.clone();
    let direct = direct.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || core.use_provider(tool, &id, direct))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn is_tool_direct(state: State<'_, AppState>, tool: ToolKind) -> CmdResult<bool> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.is_direct(tool))
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

// ----- global config -----

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> CmdResult<ConfigDto> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = core.config();
        let api_key = core.global_api_key();
        ConfigDto {
            secrets: cfg.secrets.to_string(),
            base_url: cfg.base_url,
            model: cfg.model,
            api: cfg.api,
            api_key: api_key.as_ref().map(|k| mask_key(k)),
            keychain_allow_all: cfg.keychain_allow_all,
        }
    })
    .await
    .map_err(err)
}

#[tauri::command]
pub async fn set_config(state: State<'_, AppState>, input: ConfigInput) -> CmdResult<ConfigDto> {
    let core = state.core.clone();
    let res: Result<ConfigDto, xfade_core::CoreError> = tauri::async_runtime::spawn_blocking(
        move || -> Result<ConfigDto, xfade_core::CoreError> {
            core.update_config(
                input.secrets.as_deref(),
                input.base_url.as_deref(),
                input.model.as_deref(),
                input.api.as_deref(),
            )?;
            if let Some(allow) = input.keychain_allow_all {
                core.set_keychain_allow_all(allow)?;
            }
            if let Some(k) = input.api_key.as_deref().filter(|k| !k.is_empty()) {
                core.set_global_api_key(k)?;
            }
            let cfg = core.config();
            let api_key = core.global_api_key();
            Ok(ConfigDto {
                secrets: cfg.secrets.to_string(),
                base_url: cfg.base_url,
                model: cfg.model,
                api: cfg.api,
                api_key: api_key.as_ref().map(|k| mask_key(k)),
                keychain_allow_all: cfg.keychain_allow_all,
            })
        },
    )
    .await
    .map_err(err)?;
    res.map_err(err)
}

// ----- proxy / routes / stats / logs -----

#[tauri::command]
pub async fn proxy_start(
    app: tauri::AppHandle,
    host: String,
    port: u16,
    auth_token: Option<String>,
) -> CmdResult<ProxyStatus> {
    // Run `xfade serve install` via the sidecar (the GUI self-installs the daemon, no CLI pre-install needed).
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
pub async fn proxy_stop() -> CmdResult<ProxyStatus> {
    crate::daemon_ctl::stop().await?;
    Ok(crate::daemon_ctl::status_from_config().await)
}

#[tauri::command]
pub async fn proxy_restart(app: tauri::AppHandle) -> CmdResult<ProxyStatus> {
    crate::daemon_ctl::restart(&app).await?;
    for _ in 0..8 {
        let s = crate::daemon_ctl::status_from_config().await;
        if s.running {
            return Ok(s);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(crate::daemon_ctl::status_from_config().await)
}

#[tauri::command]
pub async fn proxy_status() -> CmdResult<ProxyStatus> {
    Ok(crate::daemon_ctl::status_from_config().await)
}

#[tauri::command]
pub async fn get_routes(state: State<'_, AppState>) -> CmdResult<Option<RoutesDto>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let r = core.db().get_routes()?;
        let route_models = core.db().get_route_models()?;
        Ok::<_, xfade_core::CoreError>(r.map(|(routes, model_override, target_protocol)| {
            RoutesDto {
                routes,
                model_override,
                target_protocol,
                route_models,
            }
        }))
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
pub async fn set_routes(
    state: State<'_, AppState>,
    routes: Vec<String>,
    model_override: Option<String>,
    target_protocol: String,
    route_models: Option<BTreeMap<String, String>>,
) -> CmdResult<()> {
    let core = state.core.clone();
    let models = route_models.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        core.db().set_routes_with_models(
            &routes,
            model_override.as_deref(),
            &target_protocol,
            &models,
        )
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
    tauri::async_runtime::spawn_blocking(move || core.db().stats_since(&since, by, None))
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

// ----- probing, model map & security doctor -----

#[tauri::command]
pub async fn probe_provider_latency(
    state: State<'_, AppState>,
    tool: ToolKind,
    id: String,
) -> CmdResult<Option<u128>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let p = core
            .list(Some(tool))
            .map_err(err)?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("provider not found: {tool}/{id}"))?;
        let Some(base_url) = core.effective_base_url(p.base_url.as_deref(), p.is_official()) else {
            return Ok(None);
        };
        let Some((host, port)) = probe_target(&base_url) else {
            return Ok(None);
        };
        Ok(probe_latency(&host, port, Duration::from_secs(3)))
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn probe_provider_models(
    state: State<'_, AppState>,
    tool: ToolKind,
    id: String,
) -> CmdResult<Vec<String>> {
    let core = state.core.clone();
    let (base_url, protocol, key) = tauri::async_runtime::spawn_blocking(move || {
        let p = core
            .list(Some(tool))
            .map_err(err)?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("provider not found: {tool}/{id}"))?;
        core.resolve_probe_params(&p).map_err(err)
    })
    .await
    .map_err(err)??;
    probe_models_http(&base_url, protocol, key.as_deref(), Duration::from_secs(5)).await
}

#[tauri::command]
pub async fn probe_models_adhoc(
    base_url: String,
    api_key: Option<String>,
    protocol: Option<ProviderProtocol>,
) -> CmdResult<Vec<String>> {
    let proto = protocol.unwrap_or(ProviderProtocol::Chat);
    probe_models_http(
        base_url.trim(),
        proto,
        api_key.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        Duration::from_secs(5),
    )
    .await
}

#[tauri::command]
pub async fn get_model_map(state: State<'_, AppState>) -> CmdResult<ModelMapDto> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let raw = core.db().get_model_mappings().map_err(err)?;
        let models_merge = core.db().models_merge_enabled().map_err(err)?;
        let mut mappings = Vec::new();
        for (alias, pairs) in raw {
            for (provider_id, upstream_model) in pairs {
                mappings.push(ModelMappingDto {
                    alias: alias.clone(),
                    provider_id,
                    upstream_model,
                });
            }
        }
        Ok(ModelMapDto {
            mappings,
            models_merge,
        })
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn set_model_mapping(
    state: State<'_, AppState>,
    alias: String,
    provider_id: String,
    upstream_model: String,
) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        core.db()
            .set_model_mapping(&alias, &provider_id, &upstream_model)
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
pub async fn remove_model_mapping(
    state: State<'_, AppState>,
    alias: String,
    provider_id: Option<String>,
) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || {
        core.db()
            .remove_model_mapping(&alias, provider_id.as_deref())
            .map(|_| ())
    })
    .await
    .map_err(err)?
    .map_err(err)
}

#[tauri::command]
pub async fn set_models_merge(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.db().set_models_merge_enabled(enabled))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn security_doctor(state: State<'_, AppState>) -> CmdResult<SecurityDoctorReport> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.security_doctor())
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub async fn security_doctor_fix(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || core.security_doctor_fix())
        .await
        .map_err(err)?
        .map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xfade_core::models::ToolKind;

    #[test]
    fn list_presets_converts() {
        let v = presets_for(ToolKind::Codex)
            .into_iter()
            .map(PresetDto::from)
            .collect::<Vec<_>>();
        assert!(v.iter().any(|p| p.id == "official" && p.is_official));
        assert!(v.iter().any(|p| p.id == "local-proxy" && !p.is_official));
    }

    #[test]
    fn mask_key_handles_short_and_utf8_safely() {
        assert_eq!(mask_key("short"), "••••");
        assert_eq!(mask_key("sk-中文测试1234567890"), "sk-…7890");
        assert_eq!(mask_key("sk-ant-1234567890abcd"), "sk-…abcd");
    }

    #[test]
    fn routes_dto_roundtrips_route_models() {
        let mut route_models = BTreeMap::new();
        route_models.insert("ds".to_string(), "deepseek-chat".to_string());
        let dto = RoutesDto {
            routes: vec!["ds".to_string()],
            model_override: None,
            target_protocol: "chat".to_string(),
            route_models,
        };
        let json = serde_json::to_string(&dto).unwrap();
        let parsed: RoutesDto = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.route_models.get("ds").map(String::as_str),
            Some("deepseek-chat")
        );
    }
}
