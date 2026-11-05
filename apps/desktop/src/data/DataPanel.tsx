import { useCallback, useEffect, useState } from "react";
import { api } from "../api/client";
import type { DataOverviewDto } from "../ipc/DataOverviewDto";
import {
  type ConfirmStep,
  KINDS,
  KIND_LABEL,
  KIND_WARNING,
  arm,
  canDelete,
  confirmArgs,
  dataErrorText,
  disarm,
  idle,
  resultText,
  sizeText,
} from "./logic";

/**
 * "Your data": what the app stores on this computer and one button per kind to erase it. Mounting
 * only reads counts and sizes (no names, no content). Every delete needs two clicks (the first
 * only arms it) and the core checks a confirmation token again; nothing is ever deleted silently.
 */
export function DataPanel() {
  const [overview, setOverview] = useState<DataOverviewDto | null>(null);
  const [step, setStep] = useState<ConfirmStep>(idle);
  const [working, setWorking] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const refresh = useCallback(
    () => void api.dataOverview().then(setOverview).catch(() => setOverview(null)),
    [],
  );
  useEffect(refresh, [refresh]);

  const run = (kind: (typeof KINDS)[number]) => {
    const args = confirmArgs(step, kind);
    if (!args) return;
    setWorking(true);
    setMsg(null);
    api
      .dataDelete(args.kind, args.confirm)
      .then((r) => setMsg(resultText(r)))
      .catch((e) => setMsg(dataErrorText(e)))
      .finally(() => {
        setStep(idle);
        setWorking(false);
        refresh();
      });
  };

  return (
    <section className="space-y-2 border-t pt-3" aria-label="Your data">
      <h2 className="font-medium">Your data</h2>
      <p className="text-xs text-neutral-500">
        Everything the app stores stays on this computer. It never stores screenshots, audio or
        what you typed. Each delete below needs two clicks.
      </p>
      <table className="w-full text-left">
        <tbody>
          {KINDS.map((kind) => {
            const row = overview?.kinds.find((r) => r.kind === kind);
            const armed = step.kind === kind;
            return (
              <tr key={kind} className="align-top">
                <td className="py-1 pr-2">
                  <div>{KIND_LABEL[kind]}</div>
                  {armed && <div className="text-xs text-amber-800">{KIND_WARNING[kind]}</div>}
                </td>
                <td className="py-1 pr-2 text-neutral-600">{row ? sizeText(row) : "unknown"}</td>
                <td className="py-1">
                  {armed ? (
                    <span className="flex gap-2">
                      <button
                        type="button"
                        className="rounded border border-red-600 px-2 py-1 text-red-700 disabled:opacity-50"
                        disabled={!canDelete(working)}
                        onClick={() => run(kind)}
                      >
                        Yes, delete {KIND_LABEL[kind].toLowerCase()}
                      </button>
                      <button
                        type="button"
                        className="rounded border px-2 py-1"
                        onClick={() => setStep(disarm())}
                      >
                        Cancel
                      </button>
                    </span>
                  ) : (
                    <button
                      type="button"
                      className="rounded border px-2 py-1 disabled:opacity-50"
                      disabled={!canDelete(working)}
                      onClick={() => setStep(arm(kind))}
                    >
                      Delete...
                    </button>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {overview?.custom_models_dir && (
        <p className="text-xs text-neutral-500">
          You chose your own models folder. The app never deletes files there.
        </p>
      )}
      {overview?.diagnostic_logs && (
        <p className="text-xs text-neutral-500">Diagnostic logging is on (starts at the next app start).</p>
      )}
      {msg && (
        <div role="status" className="text-neutral-700">
          {msg}
        </div>
      )}
    </section>
  );
}
