import { useCallback, useEffect, useState } from "react";
import type { ModelMappingDto, Provider } from "../types";
import {
  getModelMap,
  listProviders,
  removeModelMapping,
  setModelMapping,
  setModelsMerge,
} from "../api";
import Toast from "./Toast";

export default function ModelMapEditor() {
  const [mappings, setMappings] = useState<ModelMappingDto[]>([]);
  const [modelsMerge, setModelsMergeState] = useState(false);
  const [providerIds, setProviderIds] = useState<string[]>([]);
  const [alias, setAlias] = useState("");
  const [providerId, setProviderId] = useState("*");
  const [upstreamModel, setUpstreamModel] = useState("");
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const load = useCallback(() => {
    Promise.all([getModelMap(), listProviders()])
      .then(([mm, providers]) => {
        setMappings(mm.mappings);
        setModelsMergeState(mm.models_merge);
        setProviderIds(
          Array.from(new Set(providers.map((p: Provider) => p.id))).sort()
        );
      })
      .catch((e) => setToast({ message: String(e), kind: "error" }));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const toggleMerge = async (enabled: boolean) => {
    try {
      await setModelsMerge(enabled);
      setModelsMergeState(enabled);
      setToast({
        message: `models-merge ${enabled ? "enabled" : "disabled"}`,
        kind: "success",
      });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const addOrUpdate = async () => {
    if (!alias.trim() || !upstreamModel.trim()) {
      setToast({ message: "Alias and upstream model are required", kind: "error" });
      return;
    }
    try {
      await setModelMapping(alias.trim(), providerId, upstreamModel.trim());
      setAlias("");
      setUpstreamModel("");
      setProviderId("*");
      load();
      setToast({ message: "Model mapping saved", kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const remove = async (m: ModelMappingDto) => {
    try {
      await removeModelMapping(m.alias, m.provider_id);
      load();
      setToast({ message: `Removed ${m.alias}`, kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const startEdit = (m: ModelMappingDto) => {
    setAlias(m.alias);
    setProviderId(m.provider_id);
    setUpstreamModel(m.upstream_model);
  };

  return (
    <div className="border rounded p-4 bg-white">
      <div className="flex items-center justify-between mb-3">
        <div>
          <h3 className="font-semibold">Model Map</h3>
          <p className="text-xs text-gray-400">
            Map client model aliases to provider-specific upstream model IDs.
          </p>
        </div>
        <label className="flex items-center gap-2 text-sm text-gray-700 cursor-pointer">
          <input
            type="checkbox"
            checked={modelsMerge}
            onChange={(e) => toggleMerge(e.target.checked)}
          />
          <span>models-merge (/v1/models aggregation)</span>
        </label>
      </div>

      <div className="flex gap-2 mb-4 flex-wrap">
        <input
          className="border rounded px-2 py-1.5 text-sm font-mono flex-1 min-w-[140px]"
          value={alias}
          onChange={(e) => setAlias(e.target.value)}
          placeholder="Client alias (e.g. claude-sonnet-4)"
        />
        <select
          className="border rounded px-2 py-1.5 text-sm font-mono w-36"
          value={providerId}
          onChange={(e) => setProviderId(e.target.value)}
        >
          <option value="*">* (all)</option>
          {providerIds.map((id) => (
            <option key={id} value={id}>
              {id}
            </option>
          ))}
        </select>
        <input
          className="border rounded px-2 py-1.5 text-sm font-mono flex-1 min-w-[140px]"
          value={upstreamModel}
          onChange={(e) => setUpstreamModel(e.target.value)}
          placeholder="Upstream model (e.g. deepseek-chat)"
        />
        <button
          className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white"
          onClick={addOrUpdate}
        >
          Save Rule
        </button>
      </div>

      {mappings.length === 0 ? (
        <div className="text-sm text-gray-400">No model mappings</div>
      ) : (
        <table className="w-full text-sm">
          <thead className="text-left text-gray-400 border-b">
            <tr>
              <th className="py-1.5 pr-2">Client Alias</th>
              <th className="py-1.5 pr-2">Provider</th>
              <th className="py-1.5 pr-2">Upstream Model</th>
              <th className="py-1.5 text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            {mappings.map((m) => (
              <tr key={`${m.alias}:${m.provider_id}`} className="border-b last:border-0">
                <td className="py-1.5 pr-2 font-mono">{m.alias}</td>
                <td className="py-1.5 pr-2 font-mono text-gray-600">{m.provider_id}</td>
                <td className="py-1.5 pr-2 font-mono">{m.upstream_model}</td>
                <td className="py-1.5 text-right space-x-1">
                  <button
                    className="text-xs px-2 py-0.5 border rounded"
                    onClick={() => startEdit(m)}
                  >
                    Edit
                  </button>
                  <button
                    className="text-xs px-2 py-0.5 border rounded text-red-600"
                    onClick={() => remove(m)}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
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
