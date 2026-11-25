import type { ReactNode } from "react";
import type { ProxyStatus, TrustState } from "../types";

export type Page = "providers" | "proxy" | "stats" | "logs" | "settings";

interface LayoutProps {
  active: Page;
  onNavigate: (p: Page) => void;
  onOpenSecurity: () => void;
  proxyStatus: ProxyStatus | null;
  trustState: TrustState | null;
  onRestartProxy: () => void;
  restartingProxy?: boolean;
  children: ReactNode;
}

const NAV: { key: Page; label: string }[] = [
  { key: "providers", label: "Providers" },
  { key: "proxy", label: "Proxy" },
  { key: "stats", label: "Stats" },
  { key: "logs", label: "Logs" },
  { key: "settings", label: "Settings" },
];

const TRUST_STYLE: Record<TrustState, { label: string; cls: string }> = {
  Guarded: {
    label: "GUARDED",
    cls: "bg-emerald-50 text-emerald-700 border-emerald-200 hover:bg-emerald-100",
  },
  Direct: {
    label: "DIRECT",
    cls: "bg-amber-50 text-amber-700 border-amber-200 hover:bg-amber-100",
  },
  Exposed: {
    label: "EXPOSED",
    cls: "bg-red-50 text-red-700 border-red-200 hover:bg-red-100",
  },
};

export default function Layout({
  active,
  onNavigate,
  onOpenSecurity,
  proxyStatus,
  trustState,
  onRestartProxy,
  restartingProxy = false,
  children,
}: LayoutProps) {
  const running = proxyStatus?.running ?? false;
  const managed = proxyStatus?.managed ?? false;
  const trust = trustState ? TRUST_STYLE[trustState] : null;

  return (
    <div className="flex h-screen w-screen bg-gray-50 text-gray-900">
      <nav className="w-44 shrink-0 border-r border-gray-200 bg-white p-3 flex flex-col gap-1">
        <div className="px-2 py-3 text-xs uppercase tracking-wide text-gray-400">
          Xfade
        </div>
        {NAV.map((n) => (
          <button
            key={n.key}
            onClick={() => onNavigate(n.key)}
            className={`text-left px-3 py-2 rounded text-sm ${
              active === n.key
                ? "bg-gray-900 text-white"
                : "hover:bg-gray-100 text-gray-700"
            }`}
          >
            {n.label}
          </button>
        ))}

        <div className="mt-auto pt-3 border-t border-gray-200 space-y-2">
          <div className="flex items-center justify-between gap-1 px-1">
            <button
              onClick={() => onNavigate("proxy")}
              className="flex items-center gap-1.5 text-xs text-gray-600 hover:text-gray-900 truncate"
              title={
                running
                  ? managed
                    ? "Proxy running (managed)"
                    : "Running (external)"
                  : "Proxy stopped"
              }
            >
              <span
                className={`inline-block w-2 h-2 rounded-full shrink-0 ${
                  running ? "bg-emerald-500" : "bg-gray-300"
                }`}
              />
              <span className="font-mono truncate">
                {running
                  ? `:${proxyStatus?.port ?? 24860}${managed ? "" : " ext"}`
                  : "Stopped"}
              </span>
            </button>
            {running && managed && (
              <button
                onClick={onRestartProxy}
                disabled={restartingProxy}
                title="Restart daemon"
                className="text-xs px-1.5 py-0.5 rounded border border-gray-200 text-gray-600 hover:bg-gray-100 disabled:opacity-40"
              >
                {restartingProxy ? "…" : "↻"}
              </button>
            )}
          </div>

          {trust && (
            <button
              onClick={onOpenSecurity}
              className={`w-full text-left px-2 py-1 rounded border text-[11px] font-medium tracking-wide ${trust.cls}`}
              title="Open Security Doctor"
            >
              Trust: {trust.label}
            </button>
          )}
        </div>
      </nav>
      <main className="flex-1 overflow-auto p-6">{children}</main>
    </div>
  );
}
