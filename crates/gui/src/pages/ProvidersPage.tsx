import { useCallback, useEffect, useState } from "react";
import type { Provider, ProviderProtocol, ToolKind } from "../types";
import {
  addProvider,
  importProvider,
  isToolDirect,
  listProviders,
  probeProviderLatency,
  probeProviderModels,
  removeProvider,
  updateProvider,
  useProvider,
} from "../api";
import ProviderForm from "../components/ProviderForm";
import BackupsPanel from "../components/BackupsPanel";
import Toast from "../components/Toast";

const TOOLS: { key: ToolKind; label: string }[] = [
  { key: "claude-code", label: "Claude Code" },
  { key: "codex", label: "Codex" },
  { key: "open-code", label: "OpenCode" },
  { key: "pi", label: "Pi" },
  { key: "oh-my-pi", label: "Oh My Pi" },
  { key: "aider", label: "Aider" },
  { key: "cline", label: "Cline" },
  { key: "hermes", label: "Hermes Agent" },
  { key: "openclaw", label: "OpenClaw" },
];

type LatencyState = { status: "probing" } | { status: "ok"; ms: number } | { status: "down" };

function latencyClass(ms: number): string {
  if (ms <= 100) return "text-cyan-700 bg-cyan-50 border-cyan-200";
  if (ms <= 300) return "text-emerald-700 bg-emerald-50 border-emerald-200";
  if (ms <= 800) return "text-amber-700 bg-amber-50 border-amber-200";
  return "text-red-700 bg-red-50 border-red-200";
}

export default function ProvidersPage({ onTrustChange }: { onTrustChange?: () => void }) {
  const [tool, setTool] = useState<ToolKind>("codex");
  const [list, setList] = useState<Provider[]>([]);
  const [directMode, setDirectMode] = useState(false);
  const [loading, setLoading] = useState(false);
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Provider | null>(null);
  const [latencies, setLatencies] = useState<Record<string, LatencyState>>({});
  const [modelCache, setModelCache] = useState<Record<string, string[]>>({});
  const [probingModels, setProbingModels] = useState<Record<string, boolean>>({});
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const refresh = useCallback(() => {
    setLoading(true);
    Promise.all([listProviders(tool), isToolDirect(tool).catch(() => false)])
      .then(([providers, isDirect]) => {
        setList(providers);
        setDirectMode(isDirect);
      })
      .catch((e) => setToast({ message: String(e), kind: "error" }))
      .finally(() => setLoading(false));
  }, [tool]);

  useEffect(() => {
    setLatencies({});
    setModelCache({});
    setProbingModels({});
    refresh();
  }, [refresh]);

  const probeOne = async (id: string) => {
    setLatencies((prev) => ({ ...prev, [id]: { status: "probing" } }));
    setProbingModels((prev) => ({ ...prev, [id]: true }));
    const [latRes, modRes] = await Promise.allSettled([
      probeProviderLatency(tool, id),
      probeProviderModels(tool, id),
    ]);
    setLatencies((prev) => ({
      ...prev,
      [id]:
        latRes.status === "fulfilled" && latRes.value !== null
          ? { status: "ok", ms: latRes.value }
          : { status: "down" },
    }));
    if (modRes.status === "fulfilled") {
      setModelCache((prev) => ({ ...prev, [id]: modRes.value }));
    }
    setProbingModels((prev) => ({ ...prev, [id]: false }));
  };

  const probeAll = async () => {
    await Promise.all(list.map((p) => probeOne(p.id)));
  };

  const switchTo = async (id: string, direct = false) => {
    if (
      direct &&
      !window.confirm(
        `Switch ${tool} to "${id}" in DIRECT mode?\n\nWarning: raw upstream API keys will be written to the tool's config file on disk, and TrustState will become DIRECT.`
      )
    ) {
      return;
    }
    try {
      await useProvider(tool, id, direct);
      setToast({
        message: `Switched to ${id}${direct ? " (direct)" : ""}`,
        kind: "success",
      });
      refresh();
      onTrustChange?.();
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const del = async (id: string) => {
    if (!window.confirm(`Delete ${id}?`)) return;
    try {
      await removeProvider(tool, id);
      refresh();
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const doImport = async () => {
    try {
      const p = await importProvider(tool);
      setToast({
        message: p ? `Imported ${p.id}` : "No config to import",
        kind: "success",
      });
      refresh();
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const onSave = async (input: {
    id: string;
    tool: ToolKind;
    base_url: string | null;
    key: string | null;
    protocol: ProviderProtocol | null;
    extra: unknown;
    official?: boolean;
  }) => {
    if (editing) {
      await updateProvider({ ...input });
    } else {
      await addProvider({ ...input });
    }
    refresh();
  };

  return (
    <div>
      <div className="flex gap-2 mb-4 flex-wrap">
        {TOOLS.map((t) => (
          <button
            key={t.key}
            onClick={() => setTool(t.key)}
            className={`px-3 py-1.5 rounded text-sm ${
              tool === t.key ? "bg-gray-900 text-white" : "bg-white border"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold">
            Providers <span className="text-gray-400 text-sm">({tool})</span>
          </h2>
          {directMode && (
            <span className="text-[11px] px-2 py-0.5 rounded border bg-amber-50 text-amber-700 border-amber-200 font-medium">
              DIRECT MODE
            </span>
          )}
        </div>
        <div className="flex gap-2">
          <button
            className="px-3 py-1.5 text-sm rounded border bg-white disabled:opacity-40"
            disabled={list.length === 0}
            onClick={probeAll}
          >
            Probe All
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded border bg-white"
            onClick={doImport}
          >
            Import from tool
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white"
            onClick={() => {
              setEditing(null);
              setFormOpen(true);
            }}
          >
            Add
          </button>
        </div>
      </div>

      {loading ? (
        <div className="text-gray-400 text-sm">Loading…</div>
      ) : list.length === 0 ? (
        <div className="text-gray-400 text-sm">No providers</div>
      ) : (
        <table className="w-full text-sm">
          <thead className="text-left text-gray-400 border-b">
            <tr>
              <th className="py-2 pr-2"></th>
              <th className="py-2 pr-2">Name</th>
              <th className="py-2 pr-2">Base URL</th>
              <th className="py-2 pr-2">Health / Catalog</th>
              <th className="py-2 pr-2 text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            {list.map((p) => {
              const lat = latencies[p.id];
              const models = modelCache[p.id];
              const isProbingModels = probingModels[p.id];
              return (
                <tr key={`${p.tool}/${p.id}`} className="border-b last:border-0">
                  <td className="py-2 pr-2">{p.is_active ? "✅" : ""}</td>
                  <td className="py-2 pr-2">
                    <div className="font-mono">{p.id}</div>
                    <div className="flex gap-1 mt-0.5">
                      {p.is_official && (
                        <span className="text-[10px] px-1.5 py-0.2 rounded bg-gray-100 text-gray-600">
                          official
                        </span>
                      )}
                      {p.protocol && (
                        <span className="text-[10px] px-1.5 py-0.2 rounded bg-gray-100 text-gray-600">
                          {p.protocol.toLowerCase()}
                        </span>
                      )}
                    </div>
                  </td>
                  <td className="py-2 pr-2 text-gray-600">
                    {p.base_url ?? "(official)"}
                  </td>
                  <td className="py-2 pr-2">
                    <div className="flex items-center gap-1.5 flex-wrap">
                      {lat?.status === "probing" && (
                        <span className="text-xs text-gray-400">probing…</span>
                      )}
                      {lat?.status === "ok" && (
                        <span
                          className={`text-xs px-1.5 py-0.5 rounded border font-mono ${latencyClass(
                            lat.ms
                          )}`}
                        >
                          ⚡ {lat.ms}ms
                        </span>
                      )}
                      {lat?.status === "down" && (
                        <span className="text-xs px-1.5 py-0.5 rounded border bg-red-50 text-red-700 border-red-200">
                          ✖ down
                        </span>
                      )}
                      {models && (
                        <span className="text-xs px-1.5 py-0.5 rounded border bg-gray-50 text-gray-700 border-gray-200 font-mono">
                          {models.length} models
                        </span>
                      )}
                      {isProbingModels && !models && (
                        <span className="text-xs text-gray-400">models…</span>
                      )}
                    </div>
                  </td>
                  <td className="py-2 pr-2 text-right space-x-1 whitespace-nowrap">
                    <button
                      className="text-xs px-2 py-0.5 rounded border"
                      onClick={() => probeOne(p.id)}
                    >
                      Probe
                    </button>
                    {!p.is_active && (
                      <>
                        <button
                          className="text-xs px-2 py-0.5 rounded border bg-gray-900 text-white border-gray-900"
                          onClick={() => switchTo(p.id, false)}
                        >
                          Switch
                        </button>
                        <button
                          className="text-xs px-2 py-0.5 rounded border text-amber-700 border-amber-300"
                          onClick={() => switchTo(p.id, true)}
                          title="Write raw API key to tool config (bypasses proxy)"
                        >
                          Direct
                        </button>
                      </>
                    )}
                    <button
                      className="text-xs px-2 py-0.5 rounded border"
                      onClick={() => {
                        setEditing(p);
                        setFormOpen(true);
                      }}
                    >
                      Edit
                    </button>
                    <button
                      className="text-xs px-2 py-0.5 rounded border text-red-600"
                      onClick={() => del(p.id)}
                    >
                      Delete
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}

      <BackupsPanel tool={tool} onRestored={refresh} />

      {formOpen && (
        <ProviderForm
          tool={tool}
          editing={editing}
          cachedModels={editing ? modelCache[editing.id] ?? [] : []}
          onClose={() => setFormOpen(false)}
          onSave={onSave}
        />
      )}

      {toast && (
        <Toast
          message={toast.message}
          kind={toast.kind}
          onClose={() => setToast(null)}
        />
      )}
    </div>
  );
}
