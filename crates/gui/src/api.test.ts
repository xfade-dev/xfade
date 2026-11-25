import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((cmd: string, _args?: unknown) => Promise.resolve(cmd)),
}));

import {
  listProviders,
  addProvider,
  useProvider,
  isToolDirect,
  proxyStart,
  proxyRestart,
  setRoutes,
  probeProviderLatency,
  probeProviderModels,
  probeModelsAdhoc,
  getModelMap,
  setModelMapping,
  removeModelMapping,
  setModelsMerge,
  securityDoctor,
  securityDoctorFix,
  stats,
} from "./api";

describe("api", () => {
  beforeEach(() => vi.clearAllMocks());

  it("listProviders calls invoke list_providers", async () => {
    const r = await listProviders();
    expect(r).toBe("list_providers");
  });

  it("addProvider calls invoke add_provider", async () => {
    const r = await addProvider({
      id: "x",
      tool: "codex",
      base_url: "http://u",
      key: "k",
      protocol: "chat",
    });
    expect(r).toBe("add_provider");
  });

  it("useProvider passes direct flag and isToolDirect calls is_tool_direct", async () => {
    expect(await useProvider("claude-code", "ds", true)).toBe("use_provider");
    expect(await isToolDirect("claude-code")).toBe("is_tool_direct");
  });

  it("proxyStart and proxyRestart call expected commands", async () => {
    expect(await proxyStart("127.0.0.1", 24860, null)).toBe("proxy_start");
    expect(await proxyRestart()).toBe("proxy_restart");
  });

  it("setRoutes passes route_models", async () => {
    expect(await setRoutes(["ds"], null, "chat", { ds: "deepseek-chat" })).toBe("set_routes");
  });

  it("probing wrappers call expected commands", async () => {
    expect(await probeProviderLatency("codex", "ds")).toBe("probe_provider_latency");
    expect(await probeProviderModels("codex", "ds")).toBe("probe_provider_models");
    expect(await probeModelsAdhoc("https://api.example.com", "sk-1", "chat")).toBe(
      "probe_models_adhoc"
    );
  });

  it("model map wrappers call expected commands", async () => {
    expect(await getModelMap()).toBe("get_model_map");
    expect(await setModelMapping("sonnet", "ds", "deepseek-chat")).toBe("set_model_mapping");
    expect(await removeModelMapping("sonnet", "ds")).toBe("remove_model_mapping");
    expect(await setModelsMerge(true)).toBe("set_models_merge");
  });

  it("security doctor wrappers call expected commands", async () => {
    expect(await securityDoctor()).toBe("security_doctor");
    expect(await securityDoctorFix()).toBe("security_doctor_fix");
  });

  it("stats calls invoke stats", async () => {
    const r = await stats("2026-01-01T00:00:00Z", "Provider");
    expect(r).toBe("stats");
  });
});
