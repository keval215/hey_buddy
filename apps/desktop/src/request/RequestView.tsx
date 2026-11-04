import { useEffect, useState } from "react";
import { badgeText, clipRaw, isCloud, isWindows, previewSrc, purposeText, shortDigest } from "./logic";
import type { RequestView } from "./logic";

/** Exactly what a request contains: shared by the preview panel and the "Last request" view. */
export function RequestFields({ r, cacheKey }: { r: RequestView; cacheKey?: string }) {
  const [raw, setRaw] = useState<{ text: string; clipped: boolean } | "loading" | "error" | null>(
    null,
  );
  const [showRaw, setShowRaw] = useState(false);
  const windows = isWindows();

  // A different request: forget the previously fetched raw body.
  useEffect(() => {
    setRaw(null);
    setShowRaw(false);
  }, [r.digest_hex]);

  useEffect(() => {
    if (!showRaw || raw !== null) return;
    setRaw("loading");
    fetch(previewSrc(r.body_url, windows, cacheKey))
      .then((res) => (res.ok ? res.text() : Promise.reject(new Error("status"))))
      .then((t) => setRaw(clipRaw(t)))
      .catch(() => setRaw("error"));
  }, [showRaw, raw, r.body_url, windows, cacheKey]);

  return (
    <div className="space-y-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span
          className={`rounded px-2 py-0.5 text-xs font-bold ${
            isCloud(r) ? "bg-red-600 text-white" : "bg-green-700 text-white"
          }`}
        >
          {badgeText(r)}
        </span>
        {purposeText(r) && (
          <span className="rounded bg-indigo-700 px-2 py-0.5 text-xs font-bold text-white">
            {purposeText(r)}
          </span>
        )}
        <span>
          {r.provider_id} / {r.model}
        </span>
        <span className="text-neutral-500">digest {shortDigest(r.digest_hex)}</span>
      </div>

      <section>
        <h2 className="font-medium">Screen text (exactly as sent)</h2>
        <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded border bg-neutral-50 p-2 font-mono text-xs">
          {r.screen_text ?? "(no screen text)"}
        </pre>
        <div className="text-neutral-600">
          Password fields hidden: {r.secure_fields_hidden}
        </div>
      </section>

      <section>
        <h2 className="font-medium">What you said</h2>
        <pre className="whitespace-pre-wrap rounded border bg-neutral-50 p-2 font-mono text-xs">
          {r.transcript}
        </pre>
      </section>

      {r.image_url && (
        <section>
          <h2 className="font-medium">Screenshot (secure fields blacked out)</h2>
          <img
            className="max-h-64 rounded border"
            alt="Redacted screenshot that is part of this request"
            src={previewSrc(r.image_url, windows, cacheKey)}
          />
        </section>
      )}

      <section>
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={showRaw} onChange={(e) => setShowRaw(e.target.checked)} />
          Show raw request (the exact bytes)
        </label>
        {showRaw && (
          <div>
            {raw === "loading" && <div className="text-neutral-500">Loading…</div>}
            {raw === "error" && <div role="alert" className="text-red-600">Raw request unavailable</div>}
            {raw !== null && raw !== "loading" && raw !== "error" && (
              <>
                {raw.clipped && (
                  <div className="text-neutral-600">
                    Only the first part is shown here; the whole body is what gets sent.
                  </div>
                )}
                <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded border bg-neutral-50 p-2 font-mono text-xs">
                  {raw.text}
                </pre>
              </>
            )}
          </div>
        )}
      </section>
    </div>
  );
}
