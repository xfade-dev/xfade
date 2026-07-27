import { useEffect } from "react";

export interface ToastProps {
  message: string;
  kind: "error" | "success";
  onClose: () => void;
}

export default function Toast({ message, kind, onClose }: ToastProps) {
  useEffect(() => {
    const t = setTimeout(onClose, 3500);
    return () => clearTimeout(t);
  }, [onClose]);
  const bg = kind === "error" ? "bg-red-600" : "bg-emerald-600";
  return (
    <div
      className={`fixed top-4 right-4 z-50 ${bg} text-white px-4 py-2 rounded shadow-lg max-w-md`}
    >
      {message}
    </div>
  );
}
