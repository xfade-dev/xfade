import { useState } from "react";
import Layout, { type Page } from "./components/Layout";

export default function App() {
  const [page, setPage] = useState<Page>("providers");
  return (
    <Layout active={page} onNavigate={setPage}>
      {page === "providers" && <div className="text-gray-500">Providers (coming)</div>}
      {page === "proxy" && <div className="text-gray-500">Proxy (coming)</div>}
      {page === "stats" && <div className="text-gray-500">Stats (coming)</div>}
      {page === "logs" && <div className="text-gray-500">Logs (coming)</div>}
    </Layout>
  );
}
