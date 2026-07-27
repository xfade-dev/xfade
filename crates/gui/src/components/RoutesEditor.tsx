import { useEffect, useState } from "react";
import type { Provider, RoutesDto } from "../types";
import { clearRoutes, getRoutes, listProviders, setRoutes as setRoutesApi } from "../api";

export default function RoutesEditor() {
  const [all, setAll] = useState<Provider[]>([]);
  const [routes, setRoutes] = useState<string[]>([]);
  const [modelOverride, setModelOverride] = useState("");
  const [targetProtocol, setTargetProtocol] = useState("chat");
  const [msg, setMsg] = useState<string | null>(null);

  const load = () => {
    listProviders().then(setAll).catch((e) => setMsg(String(e)));
    getRoutes()
      .then((r) => {
        if (r) {
          setRoutes(r.routes);
          setModelOverride(r.model_override ?? "");
          setTargetProtocol(r.target_protocol);
        }
      })
      .catch((e) => setMsg(String(e)));
  };

  useEffect(() => {
    load();
  }, []);

  const addRoute = (id: string) => {
    if (!routes.includes(id)) setRoutes([...routes, id]);
  };
  const removeRoute = (id: string) => setRoutes(routes.filter((r) => r !== id));
  const move = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= routes.length) return;
    const next = [...routes];
    [next[i], next[j]] = [next[j], next[i]];
    setRoutes(next);
  };

  const save = async () => {
    setMsg(null);
    try {
      await setRoutesApi(routes, modelOverride.trim() || null, targetProtocol);
      setMsg("已保存（运行中代理立即生效）");
    } catch (e) {
      setMsg(String(e));
    }
  };

  const clear = async () => {
    if (!window.confirm("清空路由？")) return;
    setMsg(null);
    try {
      await clearRoutes();
      setRoutes([]);
      setModelOverride("");
      setTargetProtocol("chat");
      setMsg("已清空");
    } catch (e) {
      setMsg(String(e));
    }
  };

  const candidates = all
    .map((p) => p.id)
    .filter((id) => !routes.includes(id));

  return (
    <div className="border rounded p-4 bg-white">
      <h3 className="font-semibold mb-3">路由</h3>
      <div className="mb-3">
        <div className="text-xs text-gray-500 mb-1">主 → 备 顺序</div>
        {routes.length === 0 ? (
          <div className="text-sm text-gray-400">未设置路由</div>
        ) : (
          <ol className="space-y-1">
            {routes.map((id, i) => (
              <li
                key={id}
                className="flex items-center justify-between bg-gray-50 px-2 py-1 rounded text-sm"
              >
                <span className="font-mono">
                  {i === 0 ? "★ " : `${i + 1}. `}
                  {id}
                </span>
                <span className="space-x-1">
                  <button
                    className="text-xs px-1.5 border rounded"
                    onClick={() => move(i, -1)}
                    disabled={i === 0}
                  >
                    ↑
                  </button>
                  <button
                    className="text-xs px-1.5 border rounded"
                    onClick={() => move(i, 1)}
                    disabled={i === routes.length - 1}
                  >
                    ↓
                  </button>
                  <button
                    className="text-xs px-1.5 border rounded text-red-600"
                    onClick={() => removeRoute(id)}
                  >
                    ✕
                  </button>
                </span>
              </li>
            ))}
          </ol>
        )}
      </div>

      <div className="mb-3">
        <div className="text-xs text-gray-500 mb-1">添加 provider</div>
        {candidates.length === 0 ? (
          <span className="text-sm text-gray-400">无可用 provider</span>
        ) : (
          <select
            className="border rounded px-2 py-1 text-sm"
            defaultValue=""
            onChange={(e) => {
              if (e.target.value) addRoute(e.target.value);
              e.target.value = "";
            }}
          >
            <option value="">选择 provider…</option>
            {candidates.map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
          </select>
        )}
      </div>

      <div className="grid grid-cols-2 gap-3 mb-3">
        <div>
          <label className="block text-xs text-gray-500 mb-1">model_override</label>
          <input
            className="w-full border rounded px-2 py-1 text-sm"
            value={modelOverride}
            onChange={(e) => setModelOverride(e.target.value)}
            placeholder="（透传）"
          />
        </div>
        <div>
          <label className="block text-xs text-gray-500 mb-1">target_protocol</label>
          <div className="flex gap-3 text-sm">
            {["chat", "messages"].map((t) => (
              <label key={t} className="flex items-center gap-1">
                <input
                  type="radio"
                  checked={targetProtocol === t}
                  onChange={() => setTargetProtocol(t)}
                />
                {t}
              </label>
            ))}
          </div>
        </div>
      </div>

      <div className="flex gap-2">
        <button
          className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white"
          onClick={save}
        >
          保存
        </button>
        <button
          className="px-3 py-1.5 text-sm rounded border"
          onClick={clear}
        >
          清空
        </button>
      </div>
      {msg && <div className="text-sm text-gray-600 mt-2">{msg}</div>}
    </div>
  );
}
