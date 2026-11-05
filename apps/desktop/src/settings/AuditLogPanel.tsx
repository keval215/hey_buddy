import { useCallback, useEffect, useState } from "react";
import { api } from "../api/client";
import type { AuditPageDto } from "../ipc/AuditPageDto";
import {
  type ClearStep,
  NOT_READY_TEXT,
  type Pager,
  auditErrorText,
  canNewer,
  cancelClear,
  checkExportPath,
  clickClear,
  corruptNotice,
  currentCursor,
  firstPage,
  limitNotice,
  newer,
  older,
  pageLabel,
  toRow,
} from "./audit";

const PAGE_SIZE = 25;

/**
 * Everything the agent did, newest first: when, which step, what kind of action, which control
 * (never what was typed), which app, how risky, whether it was allowed and how it ended. The log
 * stays on this computer and nothing is deleted unless you click Clear and confirm. Export is a
 * manual copy to a file you choose. All text is rendered as plain text.
 */
export function AuditLogPanel({ ready }: { ready: boolean }) {
  const [page, setPage] = useState<AuditPageDto | null>(null);
  const [pager, setPager] = useState<Pager>(firstPage);
  const [msg, setMsg] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [exportPath, setExportPath] = useState("");
  const [clear, setClear] = useState<ClearStep>("idle");

  const load = useCallback((p: Pager) => {
    void api
      .getAuditEntries(currentCursor(p), PAGE_SIZE)
      .then((v) => {
        setPage(v);
        setPager(p);
        setMsg(null);
      })
      .catch((e) => setMsg(auditErrorText(e)));
  }, []);

  useEffect(() => {
    load(firstPage);
  }, [load, ready]);

  const check = checkExportPath(exportPath);

  return (
    <section className="space-y-2 border-t pt-3">
      <div className="flex items-center gap-2">
        <span className="font-medium">Agent activity log</span>
        <button className="rounded border px-2" onClick={() => load(firstPage)}>
          Refresh
        </button>
      </div>
      <div className="text-neutral-500">
        A private record of what the agent did on this computer. It never contains what was typed,
        screen contents or passwords, and it never leaves your device.
      </div>
      {!ready && (
        <div role="alert" className="text-red-600">
          {NOT_READY_TEXT}
        </div>
      )}
      {msg && (
        <div role="alert" className="text-red-600">
          {msg}
        </div>
      )}
      {page && corruptNotice(page.corrupt_lines) && (
        <div role="status" className="text-amber-700">
          {corruptNotice(page.corrupt_lines)}
        </div>
      )}
      {page && limitNotice(page) && (
        <div role="status" className="text-amber-700">
          {limitNotice(page)}
        </div>
      )}
      {page && page.entries.length === 0 && (
        <div className="text-neutral-500">Nothing has been logged yet.</div>
      )}
      {page && page.entries.length > 0 && (
        <div className="max-h-72 overflow-auto">
          <table className="w-full text-left text-xs">
            <thead>
              <tr>
                {["Time", "Task", "Step", "Action", "Target", "App", "Level", "Decision", "Result"].map(
                  (h) => (
                    <th key={h} className="pr-2 font-medium">
                      {h}
                    </th>
                  ),
                )}
              </tr>
            </thead>
            <tbody>
              {page.entries.map((e) => {
                const r = toRow(e);
                return (
                  <tr key={r.key} className="border-t align-top">
                    <td className="pr-2 whitespace-nowrap">{r.time}</td>
                    <td className="pr-2">{r.task}</td>
                    <td className="pr-2">{r.step}</td>
                    <td className="pr-2">{r.action}</td>
                    <td className="pr-2 break-all">{r.target}</td>
                    <td className="pr-2 break-all">{r.app}</td>
                    <td className="pr-2">{r.level}</td>
                    <td className="pr-2">{r.decision}</td>
                    <td className="pr-2">{r.result}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      {page && (
        <div className="flex items-center gap-2">
          <button
            className="rounded border px-2 disabled:opacity-40"
            disabled={!canNewer(pager)}
            onClick={() => load(newer(pager))}
          >
            Newer
          </button>
          <button
            className="rounded border px-2 disabled:opacity-40"
            disabled={page.next_cursor == null}
            onClick={() => load(older(pager, page.next_cursor))}
          >
            Older
          </button>
          <span className="text-neutral-500">{pageLabel(pager, page)}</span>
        </div>
      )}

      <div className="space-y-1">
        <div className="font-medium">Export</div>
        <input
          className="w-96 rounded border px-1 font-mono"
          placeholder="Full path of a new file, for example C:\Users\me\Documents\buddy-log.jsonl"
          aria-label="Where to save the exported log"
          value={exportPath}
          maxLength={1024}
          onChange={(e) => {
            setExportPath(e.target.value);
            setNote(null);
          }}
        />
        <button
          className="ml-2 rounded border px-2 disabled:opacity-40"
          disabled={!check.ok || !ready}
          onClick={() =>
            check.ok &&
            void api
              .exportAuditLog(check.path)
              .then((r) => {
                setNote(`Saved a copy (${r.segments} log file${r.segments === 1 ? "" : "s"}).`);
                setMsg(null);
              })
              .catch((e) => {
                setNote(null);
                setMsg(auditErrorText(e));
              })
          }
        >
          Export a copy
        </button>
        {exportPath.trim() !== "" && !check.ok && (
          <div className="text-amber-700">{check.reason}</div>
        )}
        {note && (
          <div role="status" className="text-green-700">
            {note}
          </div>
        )}
        <div className="text-neutral-500">
          Creates a new file. An existing file is never overwritten, and nothing is uploaded.
        </div>
      </div>

      <div>
        {clear === "confirm" ? (
          <>
            <span className="text-amber-700">
              Delete the whole activity log? This cannot be undone. A note that it was cleared stays.
            </span>
            <button
              className="ml-2 rounded border px-2"
              onClick={() => {
                const r = clickClear(clear);
                setClear(r.step);
                if (r.send) {
                  void api
                    .clearAuditLog(true)
                    .then((d) => {
                      setNote(`Cleared (${d.removed} log file${d.removed === 1 ? "" : "s"} removed).`);
                      load(firstPage);
                    })
                    .catch((e) => setMsg(auditErrorText(e)));
                }
              }}
            >
              Yes, clear it
            </button>
            <button className="ml-2 rounded border px-2" onClick={() => setClear(cancelClear())}>
              Keep it
            </button>
          </>
        ) : (
          <button
            className="rounded border px-2 disabled:opacity-40"
            disabled={!ready}
            onClick={() => setClear(clickClear(clear).step)}
          >
            Clear log…
          </button>
        )}
      </div>
    </section>
  );
}
