import { useCallback, useEffect, useState } from "react";
import { securityDoctor, securityDoctorFix } from "../api";
import type { CheckStatus, SecurityDoctorReport, TrustState } from "../types";
import Toast from "./Toast";

const TRUST_BADGE: Record<TrustState, string> = {
  Guarded: "bg-emerald-50 text-emerald-700 border-emerald-200",
  Direct: "bg-amber-50 text-amber-700 border-amber-200",
  Exposed: "bg-red-50 text-red-700 border-red-200",
};

const CHECK_ICON: Record<CheckStatus, { icon: string; cls: string }> = {
  Pass: { icon: "✓", cls: "text-emerald-600" },
  Warn: { icon: "⚠", cls: "text-amber-600" },
  Fail: { icon: "✕", cls: "text-red-600" },
};

export default function SecurityDoctorPanel({
  onTrustChange,
}: {
  onTrustChange?: () => void;
}) {
  const [report, setReport] = useState<SecurityDoctorReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [fixing, setFixing] = useState(false);
  const [actions, setActions] = useState<string[]>([]);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const runCheck = useCallback(() => {
    setLoading(true);
    securityDoctor()
      .then((r) => {
        setReport(r);
        onTrustChange?.();
      })
      .catch((e) => setToast({ message: String(e), kind: "error" }))
      .finally(() => setLoading(false));
  }, [onTrustChange]);

  useEffect(() => {
    runCheck();
  }, [runCheck]);

  const runFix = async () => {
    if (
      !window.confirm(
        "Apply automatic security fixes?\n\nThis tightens file permissions, migrates exposed tool configs to the local gateway, and refreshes macOS Keychain ACLs (which may prompt for authorization on legacy items)."
      )
    ) {
      return;
    }
    setFixing(true);
    try {
      const log = await securityDoctorFix();
      setActions(log);
      setToast({
        message: log.length > 0 ? `Applied ${log.length} fix(es)` : "No changes needed",
        kind: "success",
      });
      runCheck();
    } catch (e) {
      setToast({ message: String(e), kind: "error" });
    } finally {
      setFixing(false);
    }
  };

  const hasFixable = report?.checks.some((c) => c.fixable) ?? false;

  return (
    <div className="space-y-4">
      <div className="border rounded p-4 bg-white">
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-3">
            <h3 className="font-semibold">Security Doctor</h3>
            {report && (
              <span
                className={`text-xs px-2 py-0.5 rounded border font-medium ${
                  TRUST_BADGE[report.overall_trust]
                }`}
              >
                {report.overall_trust.toUpperCase()}
              </span>
            )}
          </div>
          <div className="flex gap-2">
            <button
              className="px-3 py-1.5 text-sm rounded border bg-white disabled:opacity-40"
              disabled={loading || fixing}
              onClick={runCheck}
            >
              {loading ? "Checking…" : "Re-check"}
            </button>
            <button
              className="px-3 py-1.5 text-sm rounded bg-gray-900 text-white disabled:opacity-40"
              disabled={!hasFixable || fixing || loading}
              onClick={runFix}
            >
              {fixing ? "Fixing…" : "Fix Issues"}
            </button>
          </div>
        </div>

        {loading && !report ? (
          <div className="text-sm text-gray-400">Running security audit…</div>
        ) : report ? (
          <div className="space-y-4">
            {report.tool_trust.length > 0 && (
              <div>
                <div className="text-xs text-gray-500 mb-1.5">Tool Trust</div>
                <div className="flex flex-wrap gap-2">
                  {report.tool_trust.map((t) => (
                    <span
                      key={t.tool}
                      title={t.summary}
                      className={`text-xs px-2 py-1 rounded border font-mono ${
                        TRUST_BADGE[t.trust]
                      }`}
                    >
                      {t.tool}: {t.trust.toUpperCase()}
                    </span>
                  ))}
                </div>
              </div>
            )}

            <div>
              <div className="text-xs text-gray-500 mb-1.5">Audit Checks</div>
              <ul className="divide-y border rounded">
                {report.checks.map((c, idx) => {
                  const style = CHECK_ICON[c.status];
                  return (
                    <li key={`${c.name}-${idx}`} className="p-2.5 text-sm flex gap-2.5">
                      <span className={`font-bold ${style.cls}`}>{style.icon}</span>
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="font-medium">{c.name}</span>
                          {c.fixable && (
                            <span className="text-[10px] px-1.5 py-0.2 rounded bg-gray-100 text-gray-600">
                              fixable
                            </span>
                          )}
                        </div>
                        <div className="text-xs text-gray-600 mt-0.5 break-words">
                          {c.details}
                        </div>
                      </div>
                    </li>
                  );
                })}
              </ul>
            </div>
          </div>
        ) : null}
      </div>

      {actions.length > 0 && (
        <div className="border rounded p-4 bg-white">
          <div className="text-xs text-gray-500 mb-1.5">Remediation Log</div>
          <ul className="text-xs font-mono space-y-1 text-gray-700">
            {actions.map((a, i) => (
              <li key={i}>• {a}</li>
            ))}
          </ul>
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
