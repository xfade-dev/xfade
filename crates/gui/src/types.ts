export type ToolKind = "claude-code" | "codex" | "open-code";

export interface Provider {
  id: string;
  tool: ToolKind;
  base_url: string | null;
  key_ref: string;
  extra: unknown;
  is_active: boolean;
}

export interface PresetDto {
  id: string;
  label: string;
  base_url: string | null;
  extra: unknown;
  is_official: boolean;
}

export interface CircuitDto {
  provider_id: string;
  fails: number;
  cooldown_remaining_secs: number | null;
}

export interface ProxyStatus {
  running: boolean;
  port: number | null;
  host: string | null;
  auth_enabled: boolean;
  circuits: CircuitDto[];
}

export interface RoutesDto {
  routes: string[];
  model_override: string | null;
  target_protocol: string;
}

export interface StatsRow {
  group: string;
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  errors: number;
  avg_duration_ms: number;
}

export interface RequestLog {
  ts: string;
  endpoint: string;
  model: string | null;
  provider_id: string;
  status: number;
  prompt_tokens: number;
  completion_tokens: number;
  duration_ms: number;
  error: string | null;
}

export interface AddProviderInput {
  id: string;
  tool: ToolKind;
  base_url: string | null;
  key: string | null;
  extra?: unknown;
}

export interface UpdateProviderInput {
  id: string;
  tool: ToolKind;
  base_url: string | null;
  key: string | null;
  extra?: unknown;
}

export type StatsGroupBy = "Provider" | "Model";
