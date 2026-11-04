import { useCallback, useEffect, useState } from "react";
import { api, on } from "../api/client";
import type { UpdateStatusDto } from "../ipc/UpdateStatusDto";
import {
  CHECK_NOTE,
  DOWNGRADE_WARNING,
  MANIFEST_NOTE_TITLE,
  NOT_CONFIGURED_TEXT,
  advisories,
  canCheck,
  canDownload,
  canInstall,
  emptyUpdateView,
  installArgs,
  isDowngrade,
  onUpdateDone,
  onUpdateFailed,
  onUpdateProgress,
  percent,
  resultText,
  updateErrorText,
  type UpdateView,
} from "./logic";

/**
 * Updates section (ADR 0005). Mounting only reads the LOCAL status; the network is contacted
 * only when "Check for updates" is clicked, the installer is fetched only by "Download", and
 * "Install and restart" is a separate click (a downgrade needs one more confirm).
 */
export function UpdatePanel() {
  const [status, setStatus] = useState<UpdateStatusDto | null>(null);
  const [view, setView] = useState<UpdateView>(emptyUpdateView);
  const [working, setWorking] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [confirmDowngrade, setConfirmDowngrade] = useState(false);

  const refresh = useCallback(
    () => void api.updateStatus().then(setStatus).catch(() => setStatus(null)),
    [],
  );

  // Local state only: no check happens here.
  useEffect(refresh, [refresh]);

  useEffect(() => {
    const un = [
      on("update_progress", (e) => setView((v) => onUpdateProgress(v, e))),
      on("update_done", (e) => {
        setView((v) => onUpdateDone(v, e));
        refresh();
      }),
      on("update_failed", (e) => {
        setView((v) => onUpdateFailed(v, e));
        refresh();
      }),
    ];
    return () => {
      for (const u of un) void u.then((f) => f());
    };
  }, [refresh]);

  const check = () => {
    setWorking(true);
    setMsg(null);
    setConfirmDowngrade(false);
    api
      .updateCheck()
      .then(setStatus)
      .catch((e) => setMsg(updateErrorText(e)))
      .finally(() => setWorking(false));
  };

  const download = () => {
    setMsg(null);
    api.updateDownload().then(refresh).catch((e) => {
      setMsg(updateErrorText(e));
      refresh();
    });
  };

  const install = () => {
    if (!status) return;
    const args = installArgs(status, confirmDowngrade);
    if (!args) return;
    setWorking(true);
    setMsg(null);
    // On success the app exits; on failure it keeps running and shows the token's sentence.
    api
      .updateInstall(args.version, args.allowDowngrade)
      .catch((e) => setMsg(updateErrorText(e)))
      .finally(() => setWorking(false));
  };

  const pct = percent(view);
  const unconfigured = status !== null && !status.configured;
  const downgrade = status !== null && isDowngrade(status);

  return (
    <section className="space-y-2 border-t pt-3" aria-label="Updates">
      <h2 className="font-medium">Updates</h2>
      {unconfigured ? (
        <p className="text-neutral-600">{NOT_CONFIGURED_TEXT}</p>
      ) : (
        <p className="text-xs text-neutral-500">{CHECK_NOTE}</p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          className="rounded border px-2 py-1 disabled:opacity-50"
          disabled={!canCheck(status, working)}
          onClick={check}
        >
          Check for updates
        </button>
        <button
          type="button"
          className="rounded border px-2 py-1 disabled:opacity-50"
          disabled={!canDownload(status, view, working)}
          onClick={download}
        >
          Download
        </button>
        {view.progress !== null && (
          <button
            type="button"
            className="rounded border px-2 py-1"
            onClick={() => void api.updateCancel()}
          >
            Cancel
          </button>
        )}
        <button
          type="button"
          className="rounded border px-2 py-1 disabled:opacity-50"
          disabled={
            !canInstall(status, view, working) || (downgrade && !confirmDowngrade)
          }
          onClick={install}
        >
          Install and restart
        </button>
      </div>
      {downgrade && status?.downloaded && (
        <label className="flex items-start gap-2 text-amber-800">
          <input
            type="checkbox"
            checked={confirmDowngrade}
            onChange={(e) => setConfirmDowngrade(e.target.checked)}
          />
          <span>{DOWNGRADE_WARNING} I understand and want to go back to this version.</span>
        </label>
      )}
      {status && !unconfigured && <p>{resultText(status)}</p>}
      {status &&
        advisories(status).map((a) => (
          <p key={a} className="text-amber-800">
            {a}
          </p>
        ))}
      {view.progress !== null && (
        <p aria-live="polite">Downloading… {pct !== null ? `${pct}%` : ""}</p>
      )}
      {status?.downloaded && !view.progress && (
        <p>The installer is downloaded and verified. Install and restart is a separate step.</p>
      )}
      {view.error && (
        <p role="alert" className="text-red-600">
          {updateErrorText(view.error)}
        </p>
      )}
      {msg && (
        <p role="alert" className="text-red-600">
          {msg}
        </p>
      )}
      {status?.notes && status.result !== "up_to_date" && (
        <details>
          <summary>{MANIFEST_NOTE_TITLE}</summary>
          {/* Text only: never HTML, markdown or a link. */}
          <pre className="whitespace-pre-wrap break-words text-xs">{status.notes}</pre>
        </details>
      )}
    </section>
  );
}
