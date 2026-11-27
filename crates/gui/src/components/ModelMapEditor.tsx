import { useCallback, useEffect, useId, useState } from "react";
import type {
  MatchSource,
  ModelMapProfileDto,
  ModelMapTemplateDto,
  ModelMappingDto,
  Provider,
  ResolvedTemplateRule,
} from "../types";
import {
  applyModelMapTemplate,
  deleteModelMapProfile,
  getModelMap,
  listModelMapProfiles,
  listModelMapTemplates,
  listProviders,
  previewModelMapTemplate,
  removeModelMapping,
  saveModelMapProfile,
  setModelMapping,
  setModelsMerge,
  switchModelMapProfile,
} from "../api";
import Toast from "./Toast";

const SOURCE_BADGE: Record<MatchSource, { label: string; cls: string }> = {
  probe_exact: {
    label: "Probe Exact",
    cls: "bg-emerald-50 text-emerald-700 border-emerald-200",
  },
  probe_tier: {
    label: "Probe Tier",
    cls: "bg-cyan-50 text-cyan-700 border-cyan-200",
  },
  fallback: {
    label: "Fallback",
    cls: "bg-gray-100 text-gray-600 border-gray-200",
  },
  unmatched: {
    label: "Unmatched",
    cls: "bg-red-50 text-red-700 border-red-200",
  },
};

export default function ModelMapEditor() {
  const [mappings, setMappings] = useState<ModelMappingDto[]>([]);
  const [modelsMerge, setModelsMergeState] = useState(false);
  const [providerIds, setProviderIds] = useState<string[]>([]);
  const [alias, setAlias] = useState("");
  const [providerId, setProviderId] = useState("*");
  const [upstreamModel, setUpstreamModel] = useState("");
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  // Profiles state
  const [profiles, setProfiles] = useState<ModelMapProfileDto[]>([]);
  const [selectedProfile, setSelectedProfile] = useState("");
  const [saveProfileOpen, setSaveProfileOpen] = useState(false);
  const [newProfileName, setNewProfileName] = useState("");
  const [newProfileDesc, setNewProfileDesc] = useState("");

  // Template wizard state
  const [tplOpen, setTplOpen] = useState(false);
  const [templates, setTemplates] = useState<ModelMapTemplateDto[]>([]);
  const [tplId, setTplId] = useState("");
  const [tplProvider, setTplProvider] = useState("*");
  const [previewRules, setPreviewRules] = useState<ResolvedTemplateRule[]>([]);
  const [discoveredModels, setDiscoveredModels] = useState<string[]>([]);
  const [probeError, setProbeError] = useState<string | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [applying, setApplying] = useState(false);
  const datalistId = useId();

  const load = useCallback(() => {
    Promise.all([getModelMap(), listProviders(), listModelMapProfiles()])
      .then(([mm, providers, profs]) => {
        setMappings(mm.mappings);
        setModelsMergeState(mm.models_merge);
        setProviderIds(
          Array.from(new Set(providers.map((p: Provider) => p.id))).sort()
        );
        setProfiles(profs);
        setSelectedProfile((prev) => {
          if (prev && profs.some((p) => p.name === prev)) return prev;
          return profs.find((p) => p.is_active)?.name ?? profs[0]?.name ?? "";
        });
      })
      .catch((e) => setToast({ message: String(e), kind: "error" }));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const runPreview = useCallback(
    async (nextTplId: string, nextProvider: string, probe = true) => {
      if (!nextTplId) return;
      setPreviewLoading(true);
      setProbeError(null);
      try {
        const res = await previewModelMapTemplate(nextTplId, nextProvider, probe);
        setPreviewRules(res.rules);
        setDiscoveredModels(res.discovered_models);
        setProbeError(res.probe_error);
      } catch (e) {
        setToast({ message: String(e), kind: "error" });
      } finally {
        setPreviewLoading(false);
      }
    },
    []
  );

  const openTemplateWizard = async () => {
    setTplOpen(true);
    try {
      const list = templates.length > 0 ? templates : await listModelMapTemplates();
      setTemplates(list);
      const initialTpl = tplId || list[0]?.id || "";
      const initialProv = tplProvider || providerIds[0] || "*";
      setTplId(initialTpl);
      setTplProvider(initialProv);
      await runPreview(initialTpl, initialProv, initialProv !== "*");
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const applyTemplate = async (replace: boolean) => {
    const entries: ModelMappingDto[] = previewRules
      .filter((r) => r.upstream_model.trim() !== "")
      .map((r) => ({
        alias: r.client_alias,
        provider_id: r.provider_id,
        upstream_model: r.upstream_model.trim(),
      }));
    if (entries.length === 0) {
      setToast({ message: "No valid rules to apply", kind: "error" });
      return;
    }
    setApplying(true);
    try {
      const count = await applyModelMapTemplate(entries, replace);
      setTplOpen(false);
      load();
      setToast({
        message: `Applied ${count} rule(s) (${replace ? "replaced" : "merged"})`,
        kind: "success",
      });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    } finally {
      setApplying(false);
    }
  };

  const handleSaveProfile = async () => {
    const name = newProfileName.trim();
    if (!name) {
      setToast({ message: "Profile name is required", kind: "error" });
      return;
    }
    try {
      await saveModelMapProfile(name, newProfileDesc.trim());
      setSaveProfileOpen(false);
      setNewProfileName("");
      setNewProfileDesc("");
      load();
      setSelectedProfile(name);
      setToast({ message: `Saved profile "${name}"`, kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const handleSwitchProfile = async (replace: boolean) => {
    if (!selectedProfile) return;
    try {
      const count = await switchModelMapProfile(selectedProfile, replace);
      load();
      setToast({
        message: `Switched to "${selectedProfile}" (${count} rules, ${replace ? "replace" : "merge"})`,
        kind: "success",
      });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const handleDeleteProfile = async () => {
    if (!selectedProfile) return;
    if (!window.confirm(`Delete profile "${selectedProfile}"?`)) return;
    try {
      await deleteModelMapProfile(selectedProfile);
      load();
      setToast({ message: `Deleted profile "${selectedProfile}"`, kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

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
    <div className="border rounded p-4 bg-white space-y-4">
      <div className="flex items-center justify-between flex-wrap gap-2">
        <div>
          <h3 className="font-semibold">Model Map</h3>
          <p className="text-xs text-gray-400">
            Map client model aliases to provider-specific upstream model IDs.
          </p>
        </div>
        <div className="flex items-center gap-3">
          <button
            className="px-3 py-1.5 text-sm rounded border bg-white hover:bg-gray-50"
            onClick={openTemplateWizard}
          >
            Apply Template…
          </button>
          <label className="flex items-center gap-2 text-sm text-gray-700 cursor-pointer">
            <input
              type="checkbox"
              checked={modelsMerge}
              onChange={(e) => toggleMerge(e.target.checked)}
            />
            <span>models-merge (/v1/models aggregation)</span>
          </label>
        </div>
      </div>

      {/* Profile Quick-Switch Bar */}
      <div className="flex items-center gap-2 flex-wrap bg-gray-50 border rounded px-3 py-2">
        <span className="text-xs font-medium text-gray-500">Profile:</span>
        <select
          className="border rounded px-2 py-1 text-sm bg-white min-w-[180px]"
          value={selectedProfile}
          onChange={(e) => setSelectedProfile(e.target.value)}
          disabled={profiles.length === 0}
        >
          {profiles.length === 0 ? (
            <option value="">(no saved profiles)</option>
          ) : (
            profiles.map((p) => (
              <option key={p.name} value={p.name}>
                {p.is_active ? `★ ${p.name}` : p.name} ({p.rule_count} rules)
              </option>
            ))
          )}
        </select>
        <button
          className="px-2.5 py-1 text-xs rounded bg-gray-900 text-white disabled:opacity-40"
          disabled={!selectedProfile}
          onClick={() => handleSwitchProfile(true)}
          title="Replace current mappings with selected profile"
        >
          Switch (Replace)
        </button>
        <button
          className="px-2.5 py-1 text-xs rounded border bg-white disabled:opacity-40"
          disabled={!selectedProfile}
          onClick={() => handleSwitchProfile(false)}
          title="Merge selected profile into current mappings"
        >
          Merge
        </button>
        <button
          className="px-2.5 py-1 text-xs rounded border bg-white"
          onClick={() => setSaveProfileOpen((v) => !v)}
        >
          Save as Profile…
        </button>
        {selectedProfile && (
          <button
            className="px-2.5 py-1 text-xs rounded border text-red-600 bg-white ml-auto"
            onClick={handleDeleteProfile}
          >
            Delete Profile
          </button>
        )}
      </div>

      {saveProfileOpen && (
        <div className="flex items-center gap-2 flex-wrap border rounded p-3 bg-gray-50">
          <input
            className="border rounded px-2 py-1 text-sm flex-1 min-w-[140px] bg-white"
            value={newProfileName}
            onChange={(e) => setNewProfileName(e.target.value)}
            placeholder="Profile name (e.g. daily-deepseek)"
          />
          <input
            className="border rounded px-2 py-1 text-sm flex-1 min-w-[160px] bg-white"
            value={newProfileDesc}
            onChange={(e) => setNewProfileDesc(e.target.value)}
            placeholder="Description (optional)"
          />
          <button
            className="px-3 py-1 text-sm rounded bg-gray-900 text-white"
            onClick={handleSaveProfile}
          >
            Save Snapshot
          </button>
          <button
            className="px-2.5 py-1 text-sm rounded border bg-white"
            onClick={() => setSaveProfileOpen(false)}
          >
            Cancel
          </button>
        </div>
      )}

      {/* Manual Rule Editor */}
      <div className="flex gap-2 flex-wrap">
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
          {providerId !== "*" && !providerIds.includes(providerId) && (
            <option value={providerId}>{providerId}</option>
          )}
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

      {/* Template Auto-Configure Modal */}
      {tplOpen && (
        <div className="fixed inset-0 z-40 bg-black/30 flex items-center justify-center p-4">
          <div className="bg-white rounded-lg shadow-xl w-[720px] max-w-full max-h-[90vh] flex flex-col p-5">
            <div className="flex items-center justify-between mb-3">
              <h3 className="text-lg font-semibold">Apply Model Map Template</h3>
              <button
                className="text-sm text-gray-500 hover:text-gray-800"
                onClick={() => setTplOpen(false)}
              >
                ✕
              </button>
            </div>

            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3 mb-3">
              <div>
                <label className="block text-xs text-gray-500 mb-1">Template</label>
                <select
                  className="w-full border rounded px-2 py-1.5 text-sm"
                  value={tplId}
                  onChange={(e) => {
                    const next = e.target.value;
                    setTplId(next);
                    runPreview(next, tplProvider, tplProvider !== "*");
                  }}
                >
                  {templates.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.name} ({t.rule_count} rules)
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className="block text-xs text-gray-500 mb-1">Target Provider</label>
                <div className="flex gap-2">
                  <select
                    className="flex-1 border rounded px-2 py-1.5 text-sm font-mono"
                    value={tplProvider}
                    onChange={(e) => {
                      const next = e.target.value;
                      setTplProvider(next);
                      runPreview(tplId, next, next !== "*");
                    }}
                  >
                    <option value="*">* (all)</option>
                    {providerIds.map((id) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                  </select>
                  <button
                    className="px-2.5 py-1.5 text-xs border rounded bg-white disabled:opacity-40"
                    disabled={previewLoading || tplProvider === "*"}
                    onClick={() => runPreview(tplId, tplProvider, true)}
                  >
                    {previewLoading ? "Probing…" : "Re-probe"}
                  </button>
                </div>
              </div>
            </div>

            {templates.find((t) => t.id === tplId)?.description && (
              <p className="text-xs text-gray-500 mb-2">
                {templates.find((t) => t.id === tplId)?.description}
              </p>
            )}

            {probeError && (
              <div className="text-xs text-amber-700 bg-amber-50 border border-amber-200 rounded px-2.5 py-1.5 mb-2">
                Probe fallback ({probeError}) — using static vendor defaults.
              </div>
            )}

            <datalist id={datalistId}>
              {discoveredModels.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>

            <div className="flex-1 overflow-y-auto border rounded mb-4">
              {previewLoading ? (
                <div className="p-6 text-center text-sm text-gray-400">
                  Probing upstream /v1/models and resolving template…
                </div>
              ) : (
                <table className="w-full text-sm">
                  <thead className="text-left text-gray-400 border-b bg-gray-50">
                    <tr>
                      <th className="py-2 px-3">Client Alias</th>
                      <th className="py-2 px-2">Source</th>
                      <th className="py-2 px-3">Upstream Model</th>
                    </tr>
                  </thead>
                  <tbody>
                    {previewRules.map((r, idx) => {
                      const badge = SOURCE_BADGE[r.match_source];
                      return (
                        <tr key={`${r.client_alias}:${idx}`} className="border-b last:border-0">
                          <td className="py-1.5 px-3 font-mono text-xs">
                            {r.client_alias}
                            <span className="ml-1.5 text-[10px] text-gray-400">
                              ({r.tier})
                            </span>
                          </td>
                          <td className="py-1.5 px-2">
                            <span
                              className={`text-[11px] px-1.5 py-0.5 rounded border ${badge.cls}`}
                            >
                              {badge.label}
                            </span>
                          </td>
                          <td className="py-1.5 px-3">
                            <input
                              list={datalistId}
                              className="w-full border rounded px-2 py-1 text-xs font-mono"
                              value={r.upstream_model}
                              placeholder="(unmatched — enter model or leave empty to skip)"
                              onChange={(e) => {
                                const val = e.target.value;
                                setPreviewRules((prev) =>
                                  prev.map((item, i) =>
                                    i === idx ? { ...item, upstream_model: val } : item
                                  )
                                );
                              }}
                            />
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              )}
            </div>

            <div className="flex justify-end gap-2">
              <button
                className="px-3 py-1.5 text-sm rounded border"
                onClick={() => setTplOpen(false)}
                disabled={applying}
              >
                Cancel
              </button>
              <button
                className="px-3 py-1.5 text-sm rounded border bg-white disabled:opacity-40"
                disabled={applying || previewLoading}
                onClick={() => applyTemplate(false)}
              >
                Merge (Keep Existing)
              </button>
              <button
                className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white disabled:opacity-40"
                disabled={applying || previewLoading}
                onClick={() => applyTemplate(true)}
              >
                {applying ? "Applying…" : "Replace All"}
              </button>
            </div>
          </div>
        </div>
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
