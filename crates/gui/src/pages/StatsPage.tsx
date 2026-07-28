import { useEffect, useState } from "react";
import type { StatsGroupBy, StatsRow } from "../types";
import { stats as statsApi } from "../api";
import Toast from "../components/Toast";

const RANGES: { label: string; days: number }[] = [
  { label: "24h", days: 1 },
  { label: "7d", days: 7 },
  { label: "30d", days: 30 },
];

function sinceTs(days: number): string {
  return new Date(Date.now() - days * 86400000).toISOString();
}

export default function StatsPage() {
  const [days, setDays] = useState(7);
  const [by, setBy] = useState<StatsGroupBy>("Provider");
  const [rows, setRows] = useState<StatsRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  useEffect(() => {
    setLoading(true);
    statsApi(sinceTs(days), by)
      .then(setRows)
      .catch((e) => setToast({ message: String(e), kind: "error" }))
      .finally(() => setLoading(false));
  }, [days, by]);

  return (
    <div>
      <div className="flex items-center gap-4 mb-4">
        <h2 className="text-lg font-semibold">统计</h2>
        <div className="flex gap-1">
          {RANGES.map((r) => (
            <button
              key={r.label}
              onClick={() => setDays(r.days)}
              className={`px-2.5 py-1 text-sm rounded ${
                days === r.days ? "bg-gray-900 text-white" : "bg-white border"
              }`}
            >
              {r.label}
            </button>
          ))}
        </div>
        <div className="flex gap-1">
          {(["Provider", "Model"] as const).map((g) => (
            <button
              key={g}
              onClick={() => setBy(g)}
              className={`px-2.5 py-1 text-sm rounded ${
                by === g ? "bg-gray-900 text-white" : "bg-white border"
              }`}
            >
              {g}
            </button>
          ))}
        </div>
      </div>

      {loading ? (
        <div className="text-gray-400 text-sm">加载中…</div>
      ) : rows.length === 0 ? (
        <div className="text-gray-400 text-sm">该时间段内无请求</div>
      ) : (
        <table className="w-full text-sm">
          <thead className="text-left text-gray-400 border-b">
            <tr>
              <th className="py-2 pr-3">group</th>
              <th className="py-2 pr-3 text-right">requests</th>
              <th className="py-2 pr-3 text-right">prompt</th>
              <th className="py-2 pr-3 text-right">completion</th>
              <th className="py-2 pr-3 text-right">errors</th>
              <th className="py-2 pr-3 text-right">avg_ms</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.group} className="border-b last:border-0">
                <td className="py-2 pr-3 font-mono">{r.group}</td>
                <td className="py-2 pr-3 text-right">{r.requests}</td>
                <td className="py-2 pr-3 text-right">{r.prompt_tokens}</td>
                <td className="py-2 pr-3 text-right">{r.completion_tokens}</td>
                <td className="py-2 pr-3 text-right text-red-600">{r.errors}</td>
                <td className="py-2 pr-3 text-right">{r.avg_duration_ms}</td>
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
