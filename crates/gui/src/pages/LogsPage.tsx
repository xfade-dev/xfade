import { useEffect, useState } from "react";
import type { RequestLog } from "../types";
import { recentLogs } from "../api";
import Toast from "../components/Toast";

export default function LogsPage() {
  const [rows, setRows] = useState<RequestLog[]>([]);
  const [loading, setLoading] = useState(false);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const refresh = () => {
    setLoading(true);
    recentLogs(200)
      .then(setRows)
      .catch((e) => setToast({ message: String(e), kind: "error" }))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    refresh();
  }, []);

  return (
    <div>
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-lg font-semibold">请求日志（最近 200 条）</h2>
        <button
          className="px-3 py-1.5 text-sm rounded border bg-white"
          onClick={refresh}
          disabled={loading}
        >
          {loading ? "刷新中…" : "刷新"}
        </button>
      </div>

      {rows.length === 0 ? (
        <div className="text-gray-400 text-sm">无日志</div>
      ) : (
        <table className="w-full text-sm">
          <thead className="text-left text-gray-400 border-b">
            <tr>
              <th className="py-2 pr-2">时间</th>
              <th className="py-2 pr-2">endpoint</th>
              <th className="py-2 pr-2">model</th>
              <th className="py-2 pr-2">provider</th>
              <th className="py-2 pr-2 text-right">status</th>
              <th className="py-2 pr-2 text-right">tokens</th>
              <th className="py-2 pr-2 text-right">ms</th>
              <th className="py-2 pr-2">error</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => {
              const err = r.status >= 400;
              return (
                <tr
                  key={i}
                  className={`border-b last:border-0 ${err ? "bg-red-50" : ""}`}
                >
                  <td className="py-1.5 pr-2 font-mono text-xs">{r.ts}</td>
                  <td className="py-1.5 pr-2 font-mono text-xs">{r.endpoint}</td>
                  <td className="py-1.5 pr-2 text-xs">{r.model ?? "—"}</td>
                  <td className="py-1.5 pr-2 font-mono text-xs">{r.provider_id}</td>
                  <td className={`py-1.5 pr-2 text-right ${err ? "text-red-600" : ""}`}>
                    {r.status}
                  </td>
                  <td className="py-1.5 pr-2 text-right text-xs">
                    {r.prompt_tokens + r.completion_tokens}
                  </td>
                  <td className="py-1.5 pr-2 text-right text-xs">{r.duration_ms}</td>
                  <td className="py-1.5 pr-2 text-xs text-red-600">{r.error ?? ""}</td>
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
