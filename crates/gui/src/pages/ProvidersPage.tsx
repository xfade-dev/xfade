import { useCallback, useEffect, useState } from "react";
import type { Provider, ToolKind } from "../types";
import {
  addProvider,
  importProvider,
  listProviders,
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
];

export default function ProvidersPage() {
  const [tool, setTool] = useState<ToolKind>("codex");
  const [list, setList] = useState<Provider[]>([]);
  const [loading, setLoading] = useState(false);
  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<Provider | null>(null);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const refresh = useCallback(() => {
    setLoading(true);
    listProviders(tool)
      .then(setList)
      .catch((e) => setToast({ message: String(e), kind: "error" }))
      .finally(() => setLoading(false));
  }, [tool]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const switchTo = async (id: string) => {
    try {
      await useProvider(tool, id);
      setToast({ message: `Switched to ${id}`, kind: "success" });
      refresh();
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
    extra: unknown;
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
      <div className="flex gap-2 mb-4">
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
        <h2 className="text-lg font-semibold">
          Providers <span className="text-gray-400 text-sm">({tool})</span>
        </h2>
        <div className="flex gap-2">
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
              <th className="py-2 pr-2 text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            {list.map((p) => (
              <tr key={`${p.tool}/${p.id}`} className="border-b last:border-0">
                <td className="py-2 pr-2">{p.is_active ? "✅" : ""}</td>
                <td className="py-2 pr-2 font-mono">{p.id}</td>
                <td className="py-2 pr-2 text-gray-600">
                  {p.base_url ?? "(official)"}
                </td>
                <td className="py-2 pr-2 text-right space-x-1">
                  {!p.is_active && (
                    <button
                      className="text-xs px-2 py-0.5 rounded border"
                      onClick={() => switchTo(p.id)}
                    >
                      Switch
                    </button>
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
                  {!p.is_active && (
                    <button
                      className="text-xs px-2 py-0.5 rounded border text-red-600"
                      onClick={() => del(p.id)}
                    >
                      Delete
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <BackupsPanel tool={tool} />

      {formOpen && (
        <ProviderForm
          tool={tool}
          editing={editing}
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
