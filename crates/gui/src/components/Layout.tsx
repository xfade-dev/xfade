import type { ReactNode } from "react";

export type Page = "providers" | "proxy" | "stats" | "logs" | "settings";

interface LayoutProps {
  active: Page;
  onNavigate: (p: Page) => void;
  children: ReactNode;
}

const NAV: { key: Page; label: string }[] = [
  { key: "providers", label: "Providers" },
  { key: "proxy", label: "Proxy" },
  { key: "stats", label: "Stats" },
  { key: "logs", label: "Logs" },
  { key: "settings", label: "Settings" },
];

export default function Layout({ active, onNavigate, children }: LayoutProps) {
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
      </nav>
      <main className="flex-1 overflow-auto p-6">{children}</main>
    </div>
  );
}
