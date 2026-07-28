import { useEffect, useState } from "react";
import type { PresetDto, Provider, ToolKind } from "../types";
import { listPresets } from "../api";

export interface ProviderFormProps {
  tool: ToolKind;
  editing: Provider | null;
  onClose: () => void;
  onSave: (input: {
    id: string;
    tool: ToolKind;
    base_url: string | null;
    key: string | null;
    extra: unknown;
  }) => Promise<void>;
}

export default function ProviderForm({ tool, editing, onClose, onSave }: ProviderFormProps) {
  const [id, setId] = useState(editing?.id ?? "");
  const [baseUrl, setBaseUrl] = useState(editing?.base_url ?? "");
  const [key, setKey] = useState("");
  const [extraText, setExtraText] = useState(
    editing && editing.extra && typeof editing.extra === "object"
      ? JSON.stringify(editing.extra, null, 2)
      : ""
  );
  const [presets, setPresets] = useState<PresetDto[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    listPresets(tool).then(setPresets).catch(() => {});
  }, [tool]);

  const applyPreset = (p: PresetDto | null) => {
    if (!p) return;
    setId(p.id);
    setBaseUrl(p.base_url ?? "");
    setExtraText(
      p.extra && typeof p.extra === "object" ? JSON.stringify(p.extra, null, 2) : ""
    );
  };

  const submit = async () => {
    setErr(null);
    if (!id.trim()) {
      setErr("名称必填");
      return;
    }
    let extra: unknown = null;
    if (extraText.trim()) {
      try {
        extra = JSON.parse(extraText);
      } catch {
        setErr("extra 不是合法 JSON");
        return;
      }
    }
    const isOfficial = !baseUrl.trim();
    setBusy(true);
    try {
      await onSave({
        id: id.trim(),
        tool,
        base_url: isOfficial ? null : baseUrl.trim(),
        key: key.trim() || null,
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
      <div className="bg-white rounded-lg shadow-xl w-[460px] p-5">
        <h3 className="text-lg font-semibold mb-4">
          {editing ? `编辑 ${editing.id}` : "新增 Provider"}
        </h3>
        <div className="space-y-3">
          <div>
            <label className="block text-xs text-gray-500 mb-1">预设</label>
            <select
              className="w-full border rounded px-2 py-1.5 text-sm"
              defaultValue=""
              onChange={(e) =>
                applyPreset(presets.find((p) => p.id === e.target.value) ?? null)
              }
            >
              <option value="">（不使用预设）</option>
              {presets.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                  {p.is_official ? " (官方)" : ""}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="block text-xs text-gray-500 mb-1">名称</label>
            <input
              className="w-full border rounded px-2 py-1.5 text-sm"
              value={id}
              disabled={!!editing}
              onChange={(e) => setId(e.target.value)}
            />
          </div>
          <div>
            <label className="block text-xs text-gray-500 mb-1">Base URL（留空=官方）</label>
            <input
              className="w-full border rounded px-2 py-1.5 text-sm"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="https://..."
            />
          </div>
          <div>
            <label className="block text-xs text-gray-500 mb-1">
              API Key{editing ? "（留空=不修改）" : ""}
            </label>
            <input
              className="w-full border rounded px-2 py-1.5 text-sm"
              type="password"
              value={key}
              onChange={(e) => setKey(e.target.value)}
            />
          </div>
          <div>
            <label className="block text-xs text-gray-500 mb-1">extra (JSON, 可选)</label>
            <textarea
              className="w-full border rounded px-2 py-1.5 text-sm font-mono"
              rows={3}
              value={extraText}
              onChange={(e) => setExtraText(e.target.value)}
            />
          </div>
          {err && <div className="text-sm text-red-600">{err}</div>}
        </div>
        <div className="flex justify-end gap-2 mt-5">
          <button
            className="px-3 py-1.5 text-sm rounded border"
            onClick={onClose}
            disabled={busy}
          >
            取消
          </button>
          <button
            className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white disabled:opacity-50"
            onClick={submit}
            disabled={busy}
          >
            {busy ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
