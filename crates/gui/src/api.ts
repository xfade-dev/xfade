import { invoke } from "@tauri-apps/api/core";
import type {
  AddProviderInput,
  ConfigDto,
  ConfigInput,
  PresetDto,
  Provider,
  ProxyStatus,
  RequestLog,
  RoutesDto,
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

export const useProvider = (tool: ToolKind, id: string) =>
  invoke<void>("use_provider", { tool, id });

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

export const proxyStatus = () => invoke<ProxyStatus>("proxy_status");

export const getRoutes = () => invoke<RoutesDto | null>("get_routes");

export const setRoutes = (
  routes: string[],
  modelOverride: string | null,
  targetProtocol: string
) =>
  invoke<void>("set_routes", {
    routes,
    modelOverride,
    targetProtocol,
  });

export const clearRoutes = () => invoke<void>("clear_routes");

export const stats = (since: string, by: StatsGroupBy) =>
  invoke<StatsRow[]>("stats", { since, by });

export const recentLogs = (limit: number) =>
  invoke<RequestLog[]>("recent_logs", { limit });
