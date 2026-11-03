import { useCallback, useEffect, useState } from "react";
import { api, on } from "./api/client";
import type { Phase } from "./ipc/Phase";
import type { Settings } from "./ipc/Settings";
import type { Status } from "./ipc/Status";
import {
  PROFILES,
  VISION_MODES,
  buildPatch,
  errorText,
  isEmptyPatch,
  phaseText,
  shownVision,
  visionLocked,
} from "./settings/form";

export default function App() {
  const [current, setCurrent] = useState<Settings | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [phase, setPhase] = useState<Phase>("Idle");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void api.getSettings().then((s) => {
      setCurrent(s);
      setDraft(s);
    });
    void api.getStatus().then((s) => {
      setStatus(s);
      setPhase(s.phase);
    });
    const un = on("turn:state", (e) => setPhase(e.phase));
    return () => void un.then((f) => f());
  }, []);

  const save = useCallback(async () => {
    if (!current || !draft) return;
    const patch = buildPatch(current, draft);
    if (isEmptyPatch(patch)) return;
    try {
      const next = await api.updateSettings(patch);
      setCurrent(next);
      setDraft(next);
      setError(null);
    } catch (e) {
      setError(errorText(e));
      setDraft(current);
    }
  }, [current, draft]);

  if (!draft || !current) return <main className="p-4">Loading…</main>;
  const dirty = !isEmptyPatch(buildPatch(current, draft));

  return (
    <main className="space-y-4 p-4 text-sm">
      <h1 className="text-xl font-semibold">hey_buddy settings</h1>

      <label className="flex items-center gap-2">
        <input
          type="checkbox"
          checked={draft.local_only}
          onChange={(e) => setDraft({ ...draft, local_only: e.target.checked })}
        />
        Local only (never call cloud models)
      </label>

      <label className="block">
        Profile
        <select
          className="ml-2 rounded border px-1"
          value={draft.profile}
          onChange={(e) => setDraft({ ...draft, profile: e.target.value })}
        >
          {PROFILES.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </label>

      <label className="block">
        Vision
        <select
          className="ml-2 rounded border px-1"
          value={shownVision(draft)}
          disabled={visionLocked(draft.profile)}
          onChange={(e) =>
            setDraft({ ...draft, vision_mode: e.target.value as Settings["vision_mode"] })
          }
        >
          {VISION_MODES.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
        {visionLocked(draft.profile) && (
          <span className="ml-2 text-neutral-500">low-memory has no vision model</span>
        )}
      </label>

      <label className="block">
        Push-to-talk hotkey
        <input
          className="ml-2 w-48 rounded border px-1"
          value={draft.hotkey}
          onChange={(e) => setDraft({ ...draft, hotkey: e.target.value })}
        />
      </label>

      <div className="flex items-center gap-3">
        <button
          className="rounded bg-neutral-800 px-3 py-1 text-white disabled:opacity-40"
          disabled={!dirty}
          onClick={() => void save()}
        >
          Save
        </button>
        {error && <span role="alert" className="text-red-600">{error}</span>}
      </div>

      <section className="border-t pt-3 text-neutral-700">
        <div>Status: {phaseText(phase)}</div>
        <div>
          Hotkey: {status?.hotkey_registered ? "registered" : "not registered"}
        </div>
        {status?.settings_error && (
          <div role="alert" className="text-red-600">
            Settings file was unreadable; defaults in use ({status.settings_error})
          </div>
        )}
      </section>

      {import.meta.env.DEV && <DebugPoint />}
    </main>
  );
}

/** Dev builds only: send a test marker through the real overlay path. */
function DebugPoint() {
  const [x, setX] = useState("200");
  const [y, setY] = useState("200");
  const [msg, setMsg] = useState("");
  return (
    <section className="border-t pt-3">
      <div className="font-medium">Debug: point</div>
      <input className="w-20 rounded border px-1" value={x} onChange={(e) => setX(e.target.value)} />
      <input className="ml-2 w-20 rounded border px-1" value={y} onChange={(e) => setY(e.target.value)} />
      <button
        className="ml-2 rounded border px-2"
        onClick={() =>
          void api
            .debugPoint(Number(x), Number(y), "Test point")
            .then(() => setMsg(""))
            .catch((e) => setMsg(errorText(e)))
        }
      >
        Point
      </button>
      <span className="ml-2 text-neutral-500">{msg}</span>
    </section>
  );
}
