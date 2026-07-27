import { useCallback, useEffect, useState } from "react";
import type { ProxyStatus } from "../types";
import { proxyStart, proxyStatus, proxyStop } from "../api";
import RoutesEditor from "../components/RoutesEditor";
import Toast from "../components/Toast";

export default function ProxyPage() {
  const [host, setHost] = useState("127.0.0.1");
  const [port, setPort] = useState(24860);
  const [authToken, setAuthToken] = useState("");
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const fetchStatus = useCallback(() => {
    proxyStatus()
      .then(setStatus)
      .catch((e) => setToast({ message: String(e), kind: "error" }));
  }, []);

  useEffect(() => {
    fetchStatus();
  }, [fetchStatus]);

  // 运行中时每 2s 轮询（刷新熔断状态）
  useEffect(() => {
    if (!status?.running) return;
    const t = setInterval(fetchStatus, 2000);
    return () => clearInterval(t);
  }, [status?.running, fetchStatus]);

  const start = async () => {
    try {
      const s = await proxyStart(host, port, authToken.trim() || null);
      setStatus(s);
      setToast({ message: `代理已启动 :${port}`, kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const stop = async () => {
    try {
      const s = await proxyStop();
      setStatus(s);
      setToast({ message: "代理已停止", kind: "success" });
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    }
  };

  const running = status?.running ?? false;

  return (
    <div className="space-y-4">
      <div className="border rounded p-4 bg-white">
        <h2 className="text-lg font-semibold mb-3">本地代理</h2>
        <div className="flex items-center gap-3 flex-wrap">
          <div>
            <label className="block text-xs text-gray-500">Host</label>
            <input
              className="border rounded px-2 py-1 text-sm w-32"
              value={host}
              disabled={running}
              onChange={(e) => setHost(e.target.value)}
            />
          </div>
          <div>
            <label className="block text-xs text-gray-500">Port</label>
            <input
              className="border rounded px-2 py-1 text-sm w-20"
              type="number"
              value={port}
              disabled={running}
              onChange={(e) => setPort(Number(e.target.value))}
            />
          </div>
          <div className="flex-1 min-w-[180px]">
            <label className="block text-xs text-gray-500">Auth Token（可选）</label>
            <input
              className="border rounded px-2 py-1 text-sm w-full"
              type="password"
              value={authToken}
              disabled={running}
              onChange={(e) => setAuthToken(e.target.value)}
              placeholder="（不设置则不校验）"
            />
          </div>
          <div className="self-end">
            {running ? (
              <button
                className="px-4 py-1.5 text-sm rounded bg-red-600 text-white"
                onClick={stop}
              >
                停止
              </button>
            ) : (
              <button
                className="px-4 py-1.5 text-sm rounded bg-emerald-600 text-white"
                onClick={start}
              >
                启动
              </button>
            )}
          </div>
        </div>
        <div className="mt-3 flex items-center gap-2 text-sm">
          <span
            className={`inline-block w-2.5 h-2.5 rounded-full ${
              running ? "bg-emerald-500" : "bg-gray-300"
            }`}
          />
          {running
            ? `运行中 ${status?.host}:${status?.port}${status?.auth_enabled ? " (auth)" : ""}`
            : "已停止"}
        </div>

        {running && status && status.circuits.length > 0 && (
          <div className="mt-4">
            <div className="text-xs text-gray-500 mb-1">熔断状态</div>
            <table className="text-sm">
              <thead className="text-left text-gray-400">
                <tr>
                  <th className="pr-4 py-1">provider</th>
                  <th className="pr-4 py-1">fails</th>
                  <th className="pr-4 py-1">冷却剩余(s)</th>
                </tr>
              </thead>
              <tbody>
                {status.circuits.map((c) => (
                  <tr key={c.provider_id} className="font-mono">
                    <td className="pr-4 py-1">{c.provider_id}</td>
                    <td className="pr-4 py-1">{c.fails}</td>
                    <td className="pr-4 py-1">
                      {c.cooldown_remaining_secs ?? "—"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <RoutesEditor />

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
