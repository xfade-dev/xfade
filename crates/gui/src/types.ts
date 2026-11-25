export type ToolKind =
  | "claude-code"
  | "codex"
  | "open-code"
  | "pi"
  | "oh-my-pi"
  | "aider"
  | "cline"
  | "hermes"
  | "openclaw";

export type ProviderProtocol = "Chat" | "Messages" | "Responses";

export interface Provider {
  id: string;
  tool: ToolKind;
  base_url: string | null;
  key_ref: string;
  protocol?: ProviderProtocol | null;
  extra: unknown;
  is_active: boolean;
  is_official?: boolean;
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
  managed: boolean;
  port: number | null;
  host: string | null;
  auth_enabled: boolean;
  circuits: CircuitDto[];
}

export interface RoutesDto {
  routes: string[];
  model_override: string | null;
  target_protocol: string;
  route_models?: Record<string, string>;
}

export interface ModelMappingDto {
  alias: string;
  provider_id: string;
  upstream_model: string;
}

export interface ModelMapDto {
  mappings: ModelMappingDto[];
  models_merge: boolean;
}

export type TrustState = "Guarded" | "Direct" | "Exposed";
export type CheckStatus = "Pass" | "Warn" | "Fail";

export interface SecurityCheckItem {
  name: string;
  status: CheckStatus;
  details: string;
  fixable: boolean;
}

export interface ToolTrustReport {
  tool: ToolKind;
  trust: TrustState;
  summary: string;
}

export interface SecurityDoctorReport {
  overall_trust: TrustState;
  tool_trust: ToolTrustReport[];
  checks: SecurityCheckItem[];
}

export interface StatsRow {
  group: string;
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  cost_usd_micros?: number;
  errors: number;
  avg_duration_ms: number;
}

export interface RequestLog {
  ts: string;
  endpoint: string;
  model: string | null;
  provider_id: string;
  tool: string;
  status: number;
  prompt_tokens: number;
  completion_tokens: number;
  duration_ms: number;
  error: string | null;
  cost_usd_micros?: number;
}

export interface AddProviderInput {
  id: string;
  tool: ToolKind;
  base_url: string | null;
  key: string | null;
  protocol?: ProviderProtocol | null;
  extra?: unknown;
  /** Official login; when false, empty base URL / model inherit global defaults. */
  official?: boolean;
}

export interface UpdateProviderInput {
  id: string;
  tool: ToolKind;
  base_url: string | null;
  key: string | null;
  protocol?: ProviderProtocol | null;
  extra?: unknown;
}

export type StatsGroupBy = "Provider" | "Model";

export interface ConfigDto {
  secrets: string;
  base_url: string | null;
  model: string | null;
  api: string | null;
  api_key: string | null;
  keychain_allow_all: boolean;
}

export interface ConfigInput {
  secrets?: string | null;
  base_url?: string | null;
  model?: string | null;
  api?: string | null;
  api_key?: string | null;
  keychain_allow_all?: boolean | null;
}
