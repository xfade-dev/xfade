import { useEffect, useState } from "react";
import type { ToolKind } from "../types";
import { listBackups, restoreBackup } from "../api";

interface BackupsPanelProps {
  tool: ToolKind;
}

export default function BackupsPanel({ tool }: BackupsPanelProps) {
  const [open, setOpen] = useState(false);
  const [backups, setBackups] = useState<string[]>([]);
  const [err, setErr] = useState<string | null>(null);

  const refresh = () => {
    listBackups(tool)
      .then(setBackups)
      .catch((e) => setErr(String(e)));
  };

  useEffect(() => {
    if (open) refresh();
  }, [open, tool]);

  const restore = async (path: string) => {
    if (!window.confirm(`Restore ${path}? The current tool config will be overwritten.`)) return;
    setErr(null);
    try {
      await restoreBackup(tool, path);
      refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <div className="mt-6 border-t pt-4">
      <button
        className="text-sm text-gray-600 hover:text-gray-900"
        onClick={() => setOpen((o) => !o)}
      >
        {open ? "▼" : "▶"} Backups ({tool})
      </button>
      {open && (
        <div className="mt-2">
          {err && <div className="text-sm text-red-600 mb-2">{err}</div>}
          {backups.length === 0 ? (
            <div className="text-sm text-gray-400">No backups (auto-created on provider switch)</div>
          ) : (
            <ul className="space-y-1">
              {backups.map((b) => (
                <li
                  key={b}
                  className="flex items-center justify-between text-sm bg-gray-50 px-2 py-1 rounded"
                >
                  <span className="font-mono text-xs truncate mr-2">{b}</span>
                  <button
                    className="text-xs px-2 py-0.5 rounded border hover:bg-gray-100"
                    onClick={() => restore(b)}
                  >
                    Restore
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
