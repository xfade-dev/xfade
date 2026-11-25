import { useCallback, useEffect, useState } from "react";
import { proxyRestart, proxyStatus as fetchProxyStatus, securityDoctor } from "./api";
import Layout, { type Page } from "./components/Layout";
import Toast from "./components/Toast";
import ProvidersPage from "./pages/ProvidersPage";
import ProxyPage from "./pages/ProxyPage";
import StatsPage from "./pages/StatsPage";
import LogsPage from "./pages/LogsPage";
import SettingsPage, { type SettingsTab } from "./pages/SettingsPage";
import type { ProxyStatus, TrustState } from "./types";

export default function App() {
  const [page, setPage] = useState<Page>("providers");
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  const [proxyStatus, setProxyStatus] = useState<ProxyStatus | null>(null);
  const [trustState, setTrustState] = useState<TrustState | null>(null);
  const [restartingProxy, setRestartingProxy] = useState(false);
  const [toast, setToast] = useState<{ message: string; kind: "error" | "success" } | null>(null);

  const refreshProxy = useCallback(() => {
    fetchProxyStatus()
      .then(setProxyStatus)
      .catch(() => {});
  }, []);

  const refreshTrust = useCallback(() => {
    securityDoctor()
      .then((r) => setTrustState(r.overall_trust))
      .catch(() => {});
  }, []);

  const refreshGlobalStatus = useCallback(() => {
    refreshProxy();
    refreshTrust();
  }, [refreshProxy, refreshTrust]);

  useEffect(() => {
    refreshGlobalStatus();
    const proxyTimer = setInterval(() => {
      if (!document.hidden) refreshProxy();
    }, 5000);
    const trustTimer = setInterval(() => {
      if (!document.hidden) refreshTrust();
    }, 30000);
    return () => {
      clearInterval(proxyTimer);
      clearInterval(trustTimer);
    };
  }, [refreshGlobalStatus, refreshProxy, refreshTrust]);

  const handleRestartProxy = async () => {
    setRestartingProxy(true);
    try {
      const s = await proxyRestart();
      setProxyStatus(s);
    } catch (e) {
      setToast({ message: `Proxy restart failed: ${e}`, kind: "error" });
    } finally {
      setRestartingProxy(false);
    }
  };

  const openSecurity = () => {
    setSettingsTab("security");
    setPage("settings");
  };

  return (
    <Layout
      active={page}
      onNavigate={(p) => {
        if (p === "settings") setSettingsTab("general");
        setPage(p);
      }}
      onOpenSecurity={openSecurity}
      proxyStatus={proxyStatus}
      trustState={trustState}
      onRestartProxy={handleRestartProxy}
      restartingProxy={restartingProxy}
    >
      {page === "providers" && <ProvidersPage onTrustChange={refreshGlobalStatus} />}
      {page === "proxy" && <ProxyPage onStatusChange={refreshGlobalStatus} />}
      {page === "stats" && <StatsPage />}
      {page === "logs" && <LogsPage />}
      {page === "settings" && (
        <SettingsPage
          initialTab={settingsTab}
          onTrustChange={(trust) => {
            if (trust) setTrustState(trust);
            else refreshTrust();
          }}
        />
      )}
      {toast && (
        <Toast
          message={toast.message}
          kind={toast.kind}
          onClose={() => setToast(null)}
        />
      )}
    </Layout>
  );
}
