import { useCallback, useEffect, useState } from "react";
import type { Provider } from "../types";
import { clearRoutes, getRoutes, listProviders, setRoutes as setRoutesApi } from "../api";
import Toast from "./Toast";

export default function RoutesEditor() {
  const [allIds, setAllIds] = useState<string[]>([]);
  const [routes, setRoutes] = useState<string[]>([]);
  const [routeModels, setRouteModels] = useState<Record<string, string>>({});
  const [modelOverride, setModelOverride] = useState("");
  const [targetProtocol, setTargetProtocol] = useState("chat");
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const load = useCallback(() => {
    Promise.all([listProviders(), getRoutes()])
      .then(([providers, r]) => {
        const uniqueIds = Array.from(
          new Set(providers.map((p: Provider) => p.id))
        ).sort();
        setAllIds(uniqueIds);
        if (r) {
          setRoutes(r.routes);
          setRouteModels(r.route_models ?? {});
          setModelOverride(r.model_override ?? "");
          setTargetProtocol(r.target_protocol || "chat");
        }
      })
      .catch((e) => setToast({ message: String(e), kind: "error" }));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const toggle = (id: string, enabled: boolean) => {
    if (enabled) {
      if (!routes.includes(id)) setRoutes([...routes, id]);
    } else {
      setRoutes(routes.filter((r) => r !== id));
    }
  };

  const move = (id: string, dir: -1 | 1) => {
    const i = routes.indexOf(id);
    if (i < 0) return;
    const j = i + dir;
    if (j < 0 || j >= routes.length) return;
    const next = [...routes];
    [next[i], next[j]] = [next[j], next[i]];
    setRoutes(next);
  };

  const setUpstreamModel = (id: string, val: string) => {
    setRouteModels((prev) => {
      const next = { ...prev };
      if (val.trim()) {
        next[id] = val;
      } else {
        delete next[id];
      }
      return next;
    });
  };

  const save = async () => {
    try {
      const cleanedModels: Record<string, string> = {};
      for (const id of routes) {
        const m = routeModels[id]?.trim();
        if (m) cleanedModels[id] = m;
      }
      await setRoutesApi(
        routes,
        modelOverride.trim() || null,
        targetProtocol,
        cleanedModels
      );
      setToast({
        message: "Routes saved (takes effect immediately)",
        kind: "success",
      });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const clear = async () => {
    if (!window.confirm("Clear all routes?")) return;
    try {
      await clearRoutes();
      setRoutes([]);
      setRouteModels({});
      setModelOverride("");
      setTargetProtocol("chat");
      setToast({ message: "Routes cleared", kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const disabledIds = allIds.filter((id) => !routes.includes(id));
  const orderedIds = [...routes, ...disabledIds];

  return (
    <div className="border rounded p-4 bg-white">
      <div className="flex items-center justify-between mb-3">
        <div>
          <h3 className="font-semibold">Proxy Routes</h3>
          <p className="text-xs text-gray-400">
            Enable providers, set failover priority, and optionally bind a per-route upstream model.
          </p>
        </div>
        <div className="flex gap-2">
          <button
            className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white"
            onClick={save}
          >
            Save
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded border text-red-600"
            onClick={clear}
          >
            Clear
          </button>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3 mb-4">
        <div>
          <label className="block text-xs text-gray-500 mb-1">
            Target protocol
          </label>
          <select
            className="w-full border rounded px-2 py-1.5 text-sm"
            value={targetProtocol}
            onChange={(e) => setTargetProtocol(e.target.value)}
          >
            <option value="chat">chat (OpenAI)</option>
            <option value="messages">messages (Anthropic)</option>
          </select>
        </div>
        <div>
          <label className="block text-xs text-gray-500 mb-1">
            Global model override (optional)
          </label>
          <input
            className="w-full border rounded px-2 py-1.5 text-sm font-mono"
            value={modelOverride}
            onChange={(e) => setModelOverride(e.target.value)}
            placeholder="(none)"
          />
        </div>
      </div>

      {orderedIds.length === 0 ? (
        <div className="text-sm text-gray-400">No configured providers</div>
      ) : (
        <table className="w-full text-sm">
          <thead className="text-left text-gray-400 border-b">
            <tr>
              <th className="py-1.5 pr-2 w-8">On</th>
              <th className="py-1.5 pr-2">Priority</th>
              <th className="py-1.5 pr-2">Provider</th>
              <th className="py-1.5 pr-2">Upstream model</th>
              <th className="py-1.5 text-right">Order</th>
            </tr>
          </thead>
          <tbody>
            {orderedIds.map((id) => {
              const idx = routes.indexOf(id);
              const enabled = idx >= 0;
              return (
                <tr key={id} className="border-b last:border-0">
                  <td className="py-1.5 pr-2">
                    <input
                      type="checkbox"
                      checked={enabled}
                      onChange={(e) => toggle(id, e.target.checked)}
                    />
                  </td>
                  <td className="py-1.5 pr-2 text-xs">
                    {!enabled ? (
                      <span className="text-gray-400">—</span>
                    ) : idx === 0 ? (
                      <span className="px-1.5 py-0.5 rounded bg-amber-50 text-amber-700 border border-amber-200 font-medium">
                        ★ Primary
                      </span>
                    ) : (
                      <span className="text-gray-600">Fallback #{idx}</span>
                    )}
                  </td>
                  <td className="py-1.5 pr-2 font-mono">{id}</td>
                  <td className="py-1.5 pr-2">
                    <input
                      className="w-full border rounded px-2 py-1 text-xs font-mono disabled:bg-gray-50 disabled:text-gray-400"
                      value={routeModels[id] ?? ""}
                      disabled={!enabled}
                      onChange={(e) => setUpstreamModel(id, e.target.value)}
                      placeholder="(default)"
                    />
                  </td>
                  <td className="py-1.5 text-right space-x-1 whitespace-nowrap">
                    <button
                      className="text-xs px-1.5 py-0.5 border rounded disabled:opacity-30"
                      disabled={!enabled || idx === 0}
                      onClick={() => move(id, -1)}
                    >
                      ↑
                    </button>
                    <button
                      className="text-xs px-1.5 py-0.5 border rounded disabled:opacity-30"
                      disabled={!enabled || idx === routes.length - 1}
                      onClick={() => move(id, 1)}
                    >
                      ↓
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
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
