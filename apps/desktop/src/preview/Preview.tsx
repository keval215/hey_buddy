import { useCallback, useEffect, useState } from "react";
import { api, onThisWindow } from "../api/client";
import type { PreviewDto } from "../ipc/PreviewDto";
import { RequestFields } from "../request/RequestView";
import { isCloud, secondsLeft } from "../request/logic";
import { errorText } from "../settings/form";

/** Review panel: shows the exact request; nothing is sent until Send is pressed. */
export default function Preview() {
  const [dto, setDto] = useState<PreviewDto | null>(null);
  const [deadline, setDeadline] = useState(0);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const unShow = onThisWindow("turn:preview", (p) => {
      setDto(p);
      setDeadline(Date.now() + p.timeout_secs * 1000);
      setNow(Date.now());
      setBusy(false);
      setError(null);
    });
    const unClose = onThisWindow("turn:preview_closed", (c) =>
      setDto((cur) => (cur && cur.request_id === c.request_id ? null : cur)),
    );
    return () => {
      void unShow.then((f) => f());
      void unClose.then((f) => f());
    };
  }, []);

  useEffect(() => {
    if (!dto) return;
    const t = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(t);
  }, [dto]);

  const decide = useCallback(
    async (decision: "Send" | "Cancel") => {
      if (!dto || busy) return;
      setBusy(true);
      try {
        await api.previewDecision(dto.request_id, decision);
        setDto(null);
      } catch (e) {
        setError(errorText(e));
        setBusy(false);
      }
    },
    [dto, busy],
  );

  // Esc = Cancel.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") void decide("Cancel");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [decide]);

  if (!dto) {
    return <main className="p-4 text-sm text-neutral-600">Nothing is waiting for your approval.</main>;
  }
  return (
    <main className="space-y-3 p-4">
      <h1 className="text-lg font-semibold">
        {isCloud(dto) ? "Send this to the cloud?" : "Local request"}
      </h1>
      <p className="rounded border border-amber-400 bg-amber-50 p-2 text-sm">
        Nothing is sent until you press Send. Cancel, Esc, closing this window or waiting for the
        countdown sends nothing.
      </p>
      <RequestFields r={dto} />
      <div className="sticky bottom-0 flex items-center gap-3 border-t bg-white py-2">
        <button
          className="rounded bg-red-700 px-4 py-1 text-white disabled:opacity-40"
          disabled={busy}
          onClick={() => void decide("Send")}
        >
          Send
        </button>
        <button
          className="rounded border px-4 py-1 disabled:opacity-40"
          disabled={busy}
          onClick={() => void decide("Cancel")}
        >
          Cancel (Esc)
        </button>
        <span className="text-sm text-neutral-600">
          Auto-cancel in {secondsLeft(deadline, now)} s
        </span>
        {error && <span role="alert" className="text-sm text-red-600">{error}</span>}
      </div>
    </main>
  );
}
