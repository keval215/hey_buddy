import { useEffect, useState, useSyncExternalStore } from "react";
import { api, onThisWindow } from "../api/client";
import type { ApprovalLineDto } from "../ipc/ApprovalLineDto";
import { ApprovalController, type ViewState } from "./controller";
import { approveLabel, guardLeft, inert, shortDigest, titleFor } from "./logic";

const TIER_STYLE = {
  Read: "bg-green-700 text-white",
  Reversible: "bg-amber-600 text-white",
  Consequential: "bg-red-700 text-white",
} as const;

function Line({ l }: { l: ApprovalLineDto }) {
  return (
    <li className="space-y-1 rounded border p-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-xs">{l.index + 1}.</span>
        <span className="font-mono font-semibold">{inert(l.op)}</span>
        {l.target !== null && <span>&quot;{inert(l.target)}&quot;</span>}
        {l.refused ? (
          <span className="rounded bg-neutral-800 px-2 py-0.5 text-xs font-bold text-white">
            refused, will not run
          </span>
        ) : (
          <span className={`rounded px-2 py-0.5 text-xs font-bold ${TIER_STYLE[l.tier]}`}>
            {l.tier}
          </span>
        )}
        {l.ask_again && <span className="text-xs text-neutral-600">will ask again</span>}
      </div>
      {l.text !== null && (
        <div className="text-sm">
          Text: <span className="font-mono">&quot;{inert(l.text)}&quot;</span>
        </div>
      )}
    </li>
  );
}

export interface ViewProps {
  state: ViewState;
  now: number;
  onApprove: (synthetic: boolean) => void;
  onDeny: () => void;
  onKill: () => void;
}

/** Presentational: every string is rendered as a text node (no raw HTML anywhere). */
export function ApprovalView({ state, now, onApprove, onDeny, onKill }: ViewProps) {
  const cur = state.queue[0] ?? null;
  const left = guardLeft(state.shownAt, now);
  const approveOk = cur !== null && !state.busy && left === 0;
  const submit = cur?.kind === "Submit";
  return (
    <main className="space-y-3 p-4">
      {cur ? (
        <>
          <h1 className={`text-lg font-semibold ${submit ? "text-red-700" : ""}`}>
            {titleFor(cur.kind)}
          </h1>
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <span
              className={`rounded px-2 py-0.5 text-xs font-bold ${
                cur.live ? "bg-red-700 text-white" : "bg-indigo-700 text-white"
              }`}
            >
              {cur.live ? "LIVE" : "DRY RUN"}
            </span>
            {cur.changed && (
              <span className="rounded bg-amber-600 px-2 py-0.5 text-xs font-bold text-white">
                changed since approved
              </span>
            )}
            <span className="text-neutral-500">digest {shortDigest(cur.digest_hex)}</span>
            {state.queue.length > 1 && (
              <span className="text-neutral-500">{state.queue.length - 1} more waiting</span>
            )}
          </div>
          <p className="text-sm text-neutral-700">{inert(cur.summary)}</p>
          <ol className="space-y-2 text-sm">
            {cur.lines.map((l) => (
              <Line key={l.index} l={l} />
            ))}
          </ol>
        </>
      ) : (
        <p className="text-sm text-neutral-600">
          {state.ended === "killed"
            ? "Stopped. Nothing is waiting for your approval."
            : "Nothing is waiting for your approval."}
        </p>
      )}
      <div className="sticky bottom-0 flex flex-wrap items-center gap-3 border-t bg-white py-2">
        {cur && (
          <>
            <button
              type="button"
              className="rounded bg-red-700 px-4 py-1 text-white disabled:opacity-40"
              disabled={!approveOk}
              onKeyDown={(e) => {
                // Enter / Space must never approve; only a mouse click or Ctrl+Shift+A does.
                if (e.key === "Enter" || e.key === " ") e.preventDefault();
              }}
              onClick={(e) => onApprove(e.detail === 0)}
            >
              {approveLabel(left)}
            </button>
            <button
              type="button"
              className="rounded border px-4 py-1"
              onClick={() => onDeny()}
            >
              Deny (Esc)
            </button>
          </>
        )}
        <button
          type="button"
          className="ml-auto rounded bg-neutral-900 px-4 py-1 text-white"
          onClick={() => onKill()}
        >
          Stop agent
        </button>
        {state.error && (
          <span role="alert" className="text-sm text-red-600">
            {inert(state.error)}
          </span>
        )}
      </div>
    </main>
  );
}

const controller = new ApprovalController({
  displayed: (id) => api.approvalDisplayed(id),
  respond: (id, d, digest) => api.approvalRespond(id, d, digest),
  kill: () => api.killAgent(),
});

/** Approval window: shows approvals core created and lets the human answer them. */
export default function Approval() {
  const state = useSyncExternalStore(controller.subscribe, controller.getState);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const unReq = onThisWindow("approval:request", (d) => controller.onRequest(d));
    const unClosed = onThisWindow("approval:closed", (c) => controller.onClosed(c));
    // The event may have fired before this page mounted: pull what is waiting.
    void api
      .approvalReady()
      .then((list) => list.forEach((d) => controller.onRequest(d)))
      .catch(() => undefined);
    const onKey = (e: KeyboardEvent) => controller.handleKey(e);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      void unReq.then((f) => f());
      void unClosed.then((f) => f());
    };
  }, []);

  const showing = state.queue.length > 0;
  useEffect(() => {
    if (!showing) return;
    const t = setInterval(() => setNow(Date.now()), 100);
    return () => clearInterval(t);
  }, [showing]);

  return (
    <ApprovalView
      state={state}
      now={now}
      onApprove={(synthetic) => void controller.approve({ synthetic })}
      onDeny={() => void controller.deny()}
      onKill={() => void controller.kill()}
    />
  );
}
