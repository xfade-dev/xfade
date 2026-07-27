import { useState } from "react";
import Layout, { type Page } from "./components/Layout";
import ProvidersPage from "./pages/ProvidersPage";
import ProxyPage from "./pages/ProxyPage";
import StatsPage from "./pages/StatsPage";
import LogsPage from "./pages/LogsPage";

export default function App() {
  const [page, setPage] = useState<Page>("providers");
  return (
    <Layout active={page} onNavigate={setPage}>
      {page === "providers" && <ProvidersPage />}
      {page === "proxy" && <ProxyPage />}
      {page === "stats" && <StatsPage />}
      {page === "logs" && <LogsPage />}
    </Layout>
  );
}
