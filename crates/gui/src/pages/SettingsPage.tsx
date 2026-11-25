import { useCallback, useEffect, useState } from "react";
import { getConfig, setConfig } from "../api";
import type { ConfigDto, TrustState } from "../types";
import SecurityDoctorPanel from "../components/SecurityDoctorPanel";
import Toast from "../components/Toast";

export type SettingsTab = "general" | "security";

const EMPTY: ConfigDto = {
  secrets: "keyring",
  base_url: null,
  model: null,
  api: null,
  api_key: null,
  keychain_allow_all: true,
};

const API_CHOICES = ["openai-completions", "anthropic-messages"];

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

export default function SettingsPage({
  initialTab = "general",
  onTrustChange,
}: {
  initialTab?: SettingsTab;
  onTrustChange?: (trust?: TrustState) => void;
}) {
  const [tab, setTab] = useState<SettingsTab>(initialTab);
  const [cfg, setCfg] = useState<ConfigDto>(EMPTY);
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  useEffect(() => {
    setTab(initialTab);
  }, [initialTab]);

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
        api_key: apiKey.trim() || null,
        keychain_allow_all: cfg.keychain_allow_all,
      });
      setCfg(updated);
      setApiKey("");
      setToast({ message: "Saved", kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const currentApi = cfg.api ?? "";
  const hasCustomApi = currentApi !== "" && !API_CHOICES.includes(currentApi);

  return (
    <div className="max-w-2xl space-y-4">
      <div className="flex items-center justify-between">
        <div className="flex gap-2">
          <button
            onClick={() => setTab("general")}
            className={`px-3 py-1.5 rounded text-sm ${
              tab === "general" ? "bg-gray-900 text-white" : "bg-white border"
            }`}
          >
            General
          </button>
          <button
            onClick={() => setTab("security")}
            className={`px-3 py-1.5 rounded text-sm ${
              tab === "security" ? "bg-gray-900 text-white" : "bg-white border"
            }`}
          >
            Security
          </button>
        </div>
        {tab === "general" && (
          <button
            className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white"
            onClick={onSave}
          >
            Save
          </button>
        )}
      </div>

      {tab === "security" ? (
        <SecurityDoctorPanel onTrustChange={onTrustChange} />
      ) : loading ? (
        <div className="text-gray-400 text-sm">Loading…</div>
      ) : (
        <div className="border rounded p-4 bg-white space-y-4">
          <p className="text-xs text-gray-400">
            Shared defaults applied to every tool when a provider omits its own value.
            Changing the secrets backend takes effect on next start.
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

          {cfg.secrets === "keyring" && (
            <label className="flex items-start gap-2.5 text-sm text-gray-700 bg-gray-50 border rounded p-3">
              <input
                type="checkbox"
                className="mt-0.5"
                checked={cfg.keychain_allow_all}
                onChange={(e) =>
                  setCfg({ ...cfg, keychain_allow_all: e.target.checked })
                }
              />
              <div>
                <div className="font-medium">
                  Allow all local apps to read Keychain entries (macOS)
                </div>
                <div className="text-xs text-gray-500 mt-0.5">
                  Avoids repeated macOS Keychain authorization prompts when Homebrew or
                  GUI upgrades replace the binary signature.
                </div>
              </div>
            </label>
          )}

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

          <label className="block">
            <span className="text-sm text-gray-600">
              API (Pi / Oh My Pi wire protocol)
            </span>
            <select
              value={currentApi}
              onChange={(e) => setCfg({ ...cfg, api: e.target.value })}
              className="mt-1 block w-full rounded border border-gray-300 px-3 py-2 text-sm focus:border-gray-500 focus:outline-none"
            >
              <option value="">(default)</option>
              {API_CHOICES.map((choice) => (
                <option key={choice} value={choice}>
                  {choice}
                </option>
              ))}
              {hasCustomApi && <option value={currentApi}>{currentApi}</option>}
            </select>
          </label>

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
