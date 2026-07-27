import { useState } from "react";
import Layout, { type Page } from "./components/Layout";
import ProvidersPage from "./pages/ProvidersPage";
import ProxyPage from "./pages/ProxyPage";

export default function App() {
  const [page, setPage] = useState<Page>("providers");
  return (
    <Layout active={page} onNavigate={setPage}>
      {page === "providers" && <ProvidersPage />}
      {page === "proxy" && <ProxyPage />}
      {page === "stats" && <div className="text-gray-500">Stats (coming)</div>}
      {page === "logs" && <div className="text-gray-500">Logs (coming)</div>}
    </Layout>
  );
}
