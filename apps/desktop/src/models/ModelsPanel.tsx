import { useCallback, useEffect, useState } from "react";
import { api, on } from "../api/client";
import type { ModelRow } from "../ipc/ModelRow";
import {
  busy,
  canDownload,
  canLoadNow,
  diskText,
  downloadErrorText,
  downloadLabel,
  emptyView,
  licenceText,
  needsExtraConfirm,
  onDone,
  onFailed,
  onProgress,
  percent,
  statusText,
  formatBytes,
  type DownloadView,
} from "./logic";
import { swapErrorText } from "../settings/form";

/**
 * Models table. Nothing downloads without a click on a button here; a model whose checksum is
 * unknown needs a second, separate confirm. All transfers happen in the Rust core.
 */
export function ModelsPanel({
  profile,
  refreshKey = 0,
  onlyIds,
}: {
  profile: string;
  /** Show only these registry ids (the first-run wizard lists the active profile's models). */
  onlyIds?: string[];
  /** Bump to re-read the table (the models folder changed). */
  refreshKey?: number;
}) {
  const [rows, setRows] = useState<ModelRow[]>([]);
  const [view, setView] = useState<DownloadView>(emptyView);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  const refresh = useCallback(
    () => void api.listModels().then(setRows).catch(() => setRows([])),
    [],
  );

  // The profile decides which rows offer "Load now"; a new models folder changes every row.
  useEffect(refresh, [refresh, profile, refreshKey]);

  useEffect(() => {
    const un = [
      on("download_progress", (e) => setView((v) => onProgress(v, e))),
      on("download_done", (e) => {
        setView((v) => onDone(v, e));
        refresh();
      }),
      on("download_failed", (e) => {
        setView((v) => onFailed(v, e));
        refresh();
      }),
    ];
    return () => {
      for (const u of un) void u.then((f) => f());
    };
  }, [refresh]);

  const anyBusy = busy(rows);

  const start = (id: string, accept: boolean) => {
    setConfirming(null);
    setMsg(null);
    api
      .startDownload(id, accept)
      .then(refresh)
      .catch((e) => {
        setMsg(downloadErrorText(e));
        refresh();
      });
  };

  return (
    <section className="space-y-2 border-t pt-3">
      <div className="font-medium">Models</div>
      <div className="text-neutral-500">
        Nothing is downloaded until you click Download. Files are checked against their checksum
        before they are used.
      </div>
      {msg && (
        <div role="alert" className="text-red-600">
          {msg}
        </div>
      )}
      <table className="w-full text-left text-xs">
        <thead>
          <tr className="text-neutral-500">
            <th>Model</th>
            <th>Size / disk</th>
            <th>Status</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {rows
            .filter((r) => onlyIds === undefined || onlyIds.includes(r.id))
            .map((r) => {
            const p = view.progress[r.id];
            const pct = percent(p);
            const err = view.errors[r.id];
            return (
              <tr key={r.id} className="border-t align-top">
                <td className="py-1 pr-2">
                  <div className="font-mono">{r.id}</div>
                  <div className="text-neutral-500">{r.task}</div>
                  <div className="text-neutral-500">{licenceText(r)}</div>
                </td>
                <td className="py-1 pr-2">
                  <div>{r.size_text}</div>
                  <div className="text-neutral-500">{diskText(r)}</div>
                  {r.disk_needed !== null && r.disk_free !== null && r.disk_free < r.disk_needed && (
                    <div className="text-red-600">Not enough free disk space</div>
                  )}
                </td>
                <td className="py-1 pr-2">
                  <div>{statusText(r)}</div>
                  {r.status === "downloading" && (
                    <div aria-label={`progress ${r.id}`}>
                      <progress className="w-32" max={100} value={pct ?? undefined} />
                      <span className="ml-1">
                        {p ? formatBytes(p.bytes) : "starting"}
                        {pct !== null ? ` (${pct}%)` : ""}
                      </span>
                    </div>
                  )}
                  {err && (
                    <div role="alert" className="text-red-600">
                      {downloadErrorText(err)}
                    </div>
                  )}
                </td>
                <td className="py-1">
                  {r.status === "downloading" && (
                    <button
                      className="rounded border px-2"
                      onClick={() => void api.cancelDownload(r.id)}
                    >
                      Cancel
                    </button>
                  )}
                  {r.status === "missing" && confirming !== r.id && (
                    <button
                      className="rounded border px-2 disabled:opacity-40"
                      disabled={!canDownload(r, anyBusy)}
                      onClick={() =>
                        needsExtraConfirm(r) ? setConfirming(r.id) : start(r.id, false)
                      }
                    >
                      {downloadLabel(r, view)}
                    </button>
                  )}
                  {confirming === r.id && (
                    <div className="space-y-1">
                      <div className="text-amber-700">
                        Checksum unknown: this file cannot be verified. Download anyway?
                      </div>
                      <button
                        className="rounded border px-2"
                        onClick={() => start(r.id, true)}
                      >
                        Download without verification
                      </button>
                      <button
                        className="ml-1 rounded border px-2"
                        onClick={() => setConfirming(null)}
                      >
                        Back
                      </button>
                    </div>
                  )}
                  {canLoadNow(r, view) && r.swap_slot && (
                    <button
                      className="ml-1 rounded border px-2"
                      onClick={() =>
                        void api
                          .requestEngineSwap(r.swap_slot ?? "all")
                          .catch((e) => setMsg(swapErrorText(e)))
                      }
                    >
                      Load now
                    </button>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </section>
  );
}
