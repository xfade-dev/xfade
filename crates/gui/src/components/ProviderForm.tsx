import { useEffect, useId, useState } from "react";
import type { PresetDto, Provider, ProviderProtocol, ToolKind } from "../types";
import { listPresets, probeModelsAdhoc, probeProviderModels } from "../api";

export interface ProviderFormProps {
  tool: ToolKind;
  editing: Provider | null;
  cachedModels?: string[];
  onClose: () => void;
  onSave: (input: {
    id: string;
    tool: ToolKind;
    base_url: string | null;
    key: string | null;
    protocol: ProviderProtocol | null;
    extra: unknown;
    official?: boolean;
  }) => Promise<void>;
}

function readExtraObject(extra: unknown): Record<string, unknown> {
  return extra && typeof extra === "object" && !Array.isArray(extra)
    ? { ...(extra as Record<string, unknown>) }
    : {};
}

export default function ProviderForm({
  tool,
  editing,
  cachedModels = [],
  onClose,
  onSave,
}: ProviderFormProps) {
  const initialExtra = readExtraObject(editing?.extra);
  const initialModel =
    typeof initialExtra.model === "string" ? initialExtra.model : "";

  const [id, setId] = useState(editing?.id ?? "");
  const [baseUrl, setBaseUrl] = useState(editing?.base_url ?? "");
  const [key, setKey] = useState("");
  const [protocol, setProtocol] = useState<ProviderProtocol | "">(
    editing?.protocol ?? ""
  );
  const [model, setModel] = useState(initialModel);
  const [extraText, setExtraText] = useState(
    editing && editing.extra && typeof editing.extra === "object"
      ? JSON.stringify(editing.extra, null, 2)
      : ""
  );
  const [official, setOfficial] = useState(editing?.is_official ?? false);
  const [presets, setPresets] = useState<PresetDto[]>([]);
  const [probedModels, setProbedModels] = useState<string[]>(cachedModels);
  const [probing, setProbing] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const datalistId = useId();

  useEffect(() => {
    listPresets(tool).then(setPresets).catch(() => {});
  }, [tool]);

  const applyPreset = (p: PresetDto | null) => {
    if (!p) return;
    setId(p.id);
    setBaseUrl(p.base_url ?? "");
    setOfficial(p.is_official);
    const ex = readExtraObject(p.extra);
    setModel(typeof ex.model === "string" ? ex.model : "");
    setExtraText(
      p.extra && typeof p.extra === "object" ? JSON.stringify(p.extra, null, 2) : ""
    );
  };

  const probeModels = async () => {
    setErr(null);
    setProbing(true);
    try {
      const models = editing
        ? await probeProviderModels(tool, editing.id)
        : await probeModelsAdhoc(
            baseUrl,
            key.trim() || null,
            protocol || null
          );
      setProbedModels(models);
      if (models.length === 0) {
        setErr("No models returned from /v1/models");
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setProbing(false);
    }
  };

  const submit = async () => {
    setErr(null);
    if (!id.trim()) {
      setErr("Name is required");
      return;
    }
    let extraObj: Record<string, unknown> = {};
    if (extraText.trim()) {
      try {
        const parsed = JSON.parse(extraText);
        if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
          setErr("extra must be a JSON object");
          return;
        }
        extraObj = parsed as Record<string, unknown>;
      } catch {
        setErr("extra is not valid JSON");
        return;
      }
    }
    if (model.trim()) {
      extraObj.model = model.trim();
    } else {
      delete extraObj.model;
    }
    const extra: unknown = Object.keys(extraObj).length > 0 ? extraObj : null;

    setBusy(true);
    try {
      await onSave({
        id: id.trim(),
        tool,
        base_url: official ? null : baseUrl.trim() || null,
        official,
        key: key.trim() || null,
        protocol: protocol || null,
        extra,
      });
      onClose();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-40 bg-black/30 flex items-center justify-center">
      <div className="bg-white rounded-lg shadow-xl w-[480px] max-h-[90vh] overflow-y-auto p-5">
        <h3 className="text-lg font-semibold mb-4">
          {editing ? `Edit ${editing.id}` : "Add Provider"}
        </h3>
        <div className="space-y-3">
          <div>
            <label className="block text-xs text-gray-500 mb-1">Preset</label>
            <select
              className="w-full border rounded px-2 py-1.5 text-sm"
              defaultValue=""
              onChange={(e) =>
                applyPreset(presets.find((p) => p.id === e.target.value) ?? null)
              }
            >
              <option value="">(no preset)</option>
              {presets.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                  {p.is_official ? " (official)" : ""}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="block text-xs text-gray-500 mb-1">Name</label>
            <input
              className="w-full border rounded px-2 py-1.5 text-sm"
              value={id}
              disabled={!!editing}
              onChange={(e) => setId(e.target.value)}
            />
          </div>
          <div>
            <label className="block text-xs text-gray-500 mb-1">
              Base URL (empty uses global default from Settings)
            </label>
            <input
              className="w-full border rounded px-2 py-1.5 text-sm"
              value={baseUrl}
              disabled={official}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="https://..."
            />
          </div>
          <label className="flex items-center gap-2 text-sm text-gray-600">
            <input
              type="checkbox"
              checked={official}
              onChange={(e) => setOfficial(e.target.checked)}
            />
            Official login (OAuth / first-party; ignores global URL &amp; model)
          </label>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="block text-xs text-gray-500 mb-1">Protocol</label>
              <select
                className="w-full border rounded px-2 py-1.5 text-sm"
                value={protocol}
                onChange={(e) => setProtocol(e.target.value as ProviderProtocol | "")}
              >
                <option value="">auto</option>
                <option value="Chat">chat (OpenAI)</option>
                <option value="Messages">messages (Anthropic)</option>
                <option value="Responses">responses (Codex)</option>
              </select>
            </div>
            <div>
              <label className="block text-xs text-gray-500 mb-1">
                API Key{editing ? " (empty = unchanged)" : ""}
              </label>
              <input
                className="w-full border rounded px-2 py-1.5 text-sm"
                type="password"
                value={key}
                onChange={(e) => setKey(e.target.value)}
              />
            </div>
          </div>

          <div>
            <label className="block text-xs text-gray-500 mb-1">
              Model (empty = global default)
            </label>
            <div className="flex gap-2">
              <input
                list={datalistId}
                className="flex-1 border rounded px-2 py-1.5 text-sm font-mono"
                value={model}
                disabled={official}
                onChange={(e) => setModel(e.target.value)}
                placeholder="blank = global default"
              />
              <datalist id={datalistId}>
                {probedModels.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
              <button
                type="button"
                disabled={official || probing || (!editing && !baseUrl.trim())}
                onClick={probeModels}
                className="px-2.5 py-1.5 text-xs rounded border bg-white hover:bg-gray-50 disabled:opacity-40 whitespace-nowrap"
              >
                {probing ? "Probing…" : "Probe /v1/models"}
              </button>
            </div>
            {probedModels.length > 0 && (
              <div className="text-[11px] text-gray-400 mt-1">
                {probedModels.length} probed models available in dropdown
              </div>
            )}
          </div>

          <details className="border rounded p-2 bg-gray-50">
            <summary className="text-xs text-gray-600 cursor-pointer select-none">
              Advanced / Slot overrides (extra JSON)
            </summary>
            <textarea
              className="w-full border rounded px-2 py-1.5 text-sm font-mono mt-2 bg-white"
              rows={3}
              value={extraText}
              onChange={(e) => setExtraText(e.target.value)}
            />
          </details>
          {err && <div className="text-sm text-red-600">{err}</div>}
        </div>
        <div className="flex justify-end gap-2 mt-5">
          <button
            className="px-3 py-1.5 text-sm rounded border"
            onClick={onClose}
            disabled={busy}
          >
            Cancel
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white"
            onClick={submit}
            disabled={busy}
          >
            {busy ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
