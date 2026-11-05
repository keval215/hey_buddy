import { useCallback, useEffect, useState } from "react";
import { api } from "../api/client";
import type { DenyListDto } from "../ipc/DenyListDto";
import {
  type Pending,
  askRemove,
  checkAppId,
  denyErrorText,
  isPendingRemove,
  loadIssueText,
  removedText,
  splitEntries,
} from "./deny";

/**
 * Apps the buddy never looks at. Built-in entries are protected by a second confirm step; the
 * core refuses the removal without it and never lets the list become empty. Changes apply from
 * the next request and cancel any request that is waiting for your Send.
 */
export function DenyListPanel() {
  const [view, setView] = useState<DenyListDto | null>(null);
  const [text, setText] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending>(null);

  useEffect(() => {
    void api
      .getDenyList()
      .then(setView)
      .catch((e) => setMsg(denyErrorText(e)));
  }, []);

  const run = useCallback((p: Promise<DenyListDto>, done?: () => void) => {
    setPending(null);
    return p
      .then((v) => {
        setView(v);
        setMsg(null);
        done?.();
      })
      .catch((e) => setMsg(denyErrorText(e)));
  }, []);

  if (!view) {
    return (
      <section className="space-y-2 border-t pt-3">
        <div className="font-medium">Apps the buddy never looks at</div>
        {msg ? <div role="alert" className="text-red-600">{msg}</div> : <div>Loading…</div>}
      </section>
    );
  }

  const { builtin, user } = splitEntries(view);
  const check = checkAppId(text);
  const issue = loadIssueText(view.load_issue);
  const removed = removedText(view);

  return (
    <section className="space-y-2 border-t pt-3">
      <div className="font-medium">Apps the buddy never looks at</div>
      <div className="text-neutral-500">
        When one of these apps is in front, nothing from the screen is read or sent. Changes
        apply from your next request, and a request that is waiting for your Send is cancelled.
      </div>
      <div role="note" className="text-amber-700">
        {view.banking_notice}
      </div>
      {issue && <div role="alert" className="text-red-600">{issue}</div>}
      {removed && (
        <div role="status" className="text-amber-700">
          {removed}{" "}
          {pending?.kind === "reset" ? (
            <>
              Bring them all back?
              <button
                className="ml-2 rounded border px-2"
                onClick={() => void run(api.resetDenyListToDefaults(true))}
              >
                Yes, bring them back
              </button>
              <button className="ml-2 rounded border px-2" onClick={() => setPending(null)}>
                Cancel
              </button>
            </>
          ) : (
            <button
              className="rounded border px-2"
              onClick={() => setPending({ kind: "reset" })}
            >
              Restore built-in apps
            </button>
          )}
        </div>
      )}

      <div>
        <input
          className="w-64 rounded border px-1 font-mono"
          placeholder="com.example.bank or bank.exe"
          aria-label="App id to add"
          value={text}
          maxLength={200}
          onChange={(e) => setText(e.target.value)}
        />
        <button
          className="ml-2 rounded border px-2 disabled:opacity-40"
          disabled={!check.ok}
          onClick={() => check.ok && void run(api.addDenyEntry(check.id), () => setText(""))}
        >
          Add
        </button>
        {text.trim() !== "" && !check.ok && (
          <div className="text-amber-700">{check.reason}</div>
        )}
        <div className="text-neutral-500">
          Use the bundle id on macOS or the exe name on Windows. Capitals do not matter.
        </div>
      </div>
      {msg && <div role="alert" className="text-red-600">{msg}</div>}

      <div className="font-medium">Added by you ({user.length})</div>
      {user.length === 0 && <div className="text-neutral-500">None yet.</div>}
      <ul className="space-y-1">
        {user.map((e) => (
          <li key={e.id} className="flex items-center gap-2 font-mono">
            {e.id}
            <button
              className="rounded border px-2 font-sans"
              onClick={() => void run(api.removeDenyEntry(e.id, false))}
            >
              Remove
            </button>
          </li>
        ))}
      </ul>

      <div className="font-medium">Built-in ({builtin.length})</div>
      <ul className="max-h-48 space-y-1 overflow-auto">
        {builtin.map((e) => (
          <li key={e.id} className="flex flex-wrap items-center gap-2 font-mono">
            {e.id}
            <span className="rounded bg-neutral-200 px-1 font-sans text-xs">built-in</span>
            {isPendingRemove(pending, e.id) ? (
              <>
                <span className="font-sans text-amber-700">
                  Stop protecting this app? The buddy may then read it.
                </span>
                <button
                  className="rounded border px-2 font-sans"
                  onClick={() => void run(api.removeDenyEntry(e.id, true))}
                >
                  Yes, remove it
                </button>
                <button
                  className="rounded border px-2 font-sans"
                  onClick={() => setPending(null)}
                >
                  Keep it
                </button>
              </>
            ) : (
              <button
                className="rounded border px-2 font-sans"
                onClick={() => setPending(askRemove(e).pending)}
              >
                Remove…
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
