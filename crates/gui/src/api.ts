import { invoke } from "@tauri-apps/api/core";
import type {
  AddProviderInput,
  ConfigDto,
  ConfigInput,
  ModelMapDto,
  PresetDto,
  Provider,
  ProviderProtocol,
  ProxyStatus,
  RequestLog,
  RoutesDto,
  SecurityDoctorReport,
  StatsGroupBy,
  StatsRow,
  ToolKind,
  UpdateProviderInput,
} from "./types";

export const listProviders = (tool?: ToolKind) =>
  invoke<Provider[]>("list_providers", { tool: tool ?? null });

export const addProvider = (input: AddProviderInput) =>
  invoke<void>("add_provider", { input });

export const updateProvider = (input: UpdateProviderInput) =>
  invoke<void>("update_provider", { input });

export const removeProvider = (tool: ToolKind, id: string) =>
  invoke<void>("remove_provider", { tool, id });

export const useProvider = (tool: ToolKind, id: string, direct = false) =>
  invoke<void>("use_provider", { tool, id, direct });

export const isToolDirect = (tool: ToolKind) =>
  invoke<boolean>("is_tool_direct", { tool });

export const currentProvider = (tool: ToolKind) =>
  invoke<Provider | null>("current_provider", { tool });

export const listPresets = (tool: ToolKind) =>
  invoke<PresetDto[]>("list_presets", { tool });

export const importProvider = (tool: ToolKind) =>
  invoke<Provider | null>("import_provider", { tool });

export const listBackups = (tool: ToolKind) =>
  invoke<string[]>("list_backups", { tool });

export const restoreBackup = (tool: ToolKind, path: string) =>
  invoke<void>("restore_backup", { tool, path });

export const getConfig = () => invoke<ConfigDto>("get_config");

export const setConfig = (input: ConfigInput) =>
  invoke<ConfigDto>("set_config", { input });

export const proxyStart = (host: string, port: number, authToken: string | null) =>
  invoke<ProxyStatus>("proxy_start", { host, port, authToken });

export const proxyStop = () => invoke<ProxyStatus>("proxy_stop");

export const proxyRestart = () => invoke<ProxyStatus>("proxy_restart");

export const proxyStatus = () => invoke<ProxyStatus>("proxy_status");

export const getRoutes = () => invoke<RoutesDto | null>("get_routes");

export const setRoutes = (
  routes: string[],
  modelOverride: string | null,
  targetProtocol: string,
  routeModels?: Record<string, string>
) =>
  invoke<void>("set_routes", {
    routes,
    modelOverride,
    targetProtocol,
    routeModels: routeModels ?? null,
  });

export const clearRoutes = () => invoke<void>("clear_routes");

export const stats = (since: string, by: StatsGroupBy) =>
  invoke<StatsRow[]>("stats", { since, by });

export const recentLogs = (limit: number) =>
  invoke<RequestLog[]>("recent_logs", { limit });

export const probeProviderLatency = (tool: ToolKind, id: string) =>
  invoke<number | null>("probe_provider_latency", { tool, id });

export const probeProviderModels = (tool: ToolKind, id: string) =>
  invoke<string[]>("probe_provider_models", { tool, id });

export const probeModelsAdhoc = (
  baseUrl: string,
  apiKey: string | null,
  protocol: ProviderProtocol | null
) => invoke<string[]>("probe_models_adhoc", { baseUrl, apiKey, protocol });

export const getModelMap = () => invoke<ModelMapDto>("get_model_map");

export const setModelMapping = (alias: string, providerId: string, upstreamModel: string) =>
  invoke<void>("set_model_mapping", { alias, providerId, upstreamModel });

export const removeModelMapping = (alias: string, providerId?: string) =>
  invoke<void>("remove_model_mapping", { alias, providerId: providerId ?? null });

export const setModelsMerge = (enabled: boolean) =>
  invoke<void>("set_models_merge", { enabled });

export const securityDoctor = () => invoke<SecurityDoctorReport>("security_doctor");

export const securityDoctorFix = () => invoke<string[]>("security_doctor_fix");
