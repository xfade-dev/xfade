import { useCallback, useEffect, useState } from "react";
import { getConfig, setConfig } from "../api";
import type { ConfigDto } from "../types";
import Toast from "../components/Toast";

const EMPTY: ConfigDto = {
  secrets: "keyring",
  base_url: null,
  model: null,
  api: null,
  api_key: null,
};

interface FieldProps {
  label: string;
  value: string;
  placeholder?: string;
  onChange: (v: string) => void;
}

function Field({ label, value, placeholder, onChange }: FieldProps) {
  return (
    <label className="block">
      <span className="text-sm text-gray-600">{label}</span>
      <input
        type="text"
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        className="mt-1 block w-full rounded border border-gray-300 px-3 py-2 text-sm focus:border-gray-500 focus:outline-none"
      />
    </label>
  );
}

export default function SettingsPage() {
  const [cfg, setCfg] = useState<ConfigDto>(EMPTY);
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const refresh = useCallback(() => {
    setLoading(true);
    getConfig()
      .then(setCfg)
      .catch((e) => setToast({ message: String(e), kind: "error" }))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const onSave = async () => {
    try {
      const updated = await setConfig({
        secrets: cfg.secrets,
        base_url: cfg.base_url ?? "",
        model: cfg.model ?? "",
        api: cfg.api ?? "",
        api_key: apiKey ? apiKey : null,
      });
      setCfg(updated);
      setApiKey("");
      setToast({ message: "Saved", kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  return (
    <div className="max-w-xl">
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-lg font-semibold">Settings</h2>
        <button
          className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white"
          onClick={onSave}
        >
          Save
        </button>
      </div>

      {loading ? (
        <div className="text-gray-400 text-sm">Loading…</div>
      ) : (
        <div className="space-y-4">
          <p className="text-xs text-gray-400">
            Shared defaults applied to every tool when a provider omits its own value
            (like CC Switch&apos;s general config). Changing the secrets backend takes
            effect on next start.
          </p>

          <label className="block">
            <span className="text-sm text-gray-600">Secret storage backend</span>
            <select
              value={cfg.secrets}
              onChange={(e) => setCfg({ ...cfg, secrets: e.target.value })}
              className="mt-1 block w-full rounded border border-gray-300 px-3 py-2 text-sm focus:border-gray-500 focus:outline-none"
            >
              <option value="keyring">keyring (system keychain)</option>
              <option value="file">file (secrets.json, 0600)</option>
            </select>
          </label>

          <Field
            label="Base URL"
            value={cfg.base_url ?? ""}
            placeholder="http://your-gateway:3000"
            onChange={(v) => setCfg({ ...cfg, base_url: v })}
          />
          <Field
            label="Model"
            value={cfg.model ?? ""}
            placeholder="glm-5-2-260617"
            onChange={(v) => setCfg({ ...cfg, model: v })}
          />
          <Field
            label="API (Pi / Oh My Pi wire protocol)"
            value={cfg.api ?? ""}
            placeholder="openai-completions | anthropic-messages"
            onChange={(v) => setCfg({ ...cfg, api: v })}
          />

          <label className="block">
            <span className="text-sm text-gray-600">
              Global API key {cfg.api_key ? `(set: ${cfg.api_key})` : "(unset)"}
            </span>
            <input
              type="password"
              value={apiKey}
              placeholder="leave empty to keep unchanged"
              onChange={(e) => setApiKey(e.target.value)}
              className="mt-1 block w-full rounded border border-gray-300 px-3 py-2 text-sm focus:border-gray-500 focus:outline-none"
            />
          </label>
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
