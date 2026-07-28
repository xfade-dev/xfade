import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((cmd: string, _args?: unknown) => Promise.resolve(cmd)),
}));

import { listProviders, addProvider, proxyStart, stats } from "./api";

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
    });
    expect(r).toBe("add_provider");
  });

  it("proxyStart calls invoke proxy_start", async () => {
    const r = await proxyStart("127.0.0.1", 24860, null);
    expect(r).toBe("proxy_start");
  });

  it("stats calls invoke stats", async () => {
    const r = await stats("2026-01-01T00:00:00Z", "Provider");
    expect(r).toBe("stats");
  });
});
