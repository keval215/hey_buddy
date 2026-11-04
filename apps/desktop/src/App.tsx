import { useCallback, useEffect, useState } from "react";
import { api, on } from "./api/client";
import type { Phase } from "./ipc/Phase";
import type { LastRequestDto } from "./ipc/LastRequestDto";
import type { Settings } from "./ipc/Settings";
import { ModelsPanel } from "./models/ModelsPanel";
import { RequestFields } from "./request/RequestView";
import type { Status } from "./ipc/Status";
import {
  PROFILES,
  PROVIDERS,
  VISION_MODES,
  buildPatch,
  errorText,
  isEmptyPatch,
  keyErrorText,
  keyStatusText,
  needsEngineSwap,
  phaseText,
  shownVision,
  swapErrorText,
  swapText,
  visionLocked,
  applySwap,
  type SwapStates,
} from "./settings/form";

export default function App() {
  const [current, setCurrent] = useState<Settings | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [phase, setPhase] = useState<Phase>("Idle");
  const [error, setError] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const [swaps, setSwaps] = useState<SwapStates>({});

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
    // Fixed messages from the core, never content. Cleared when the next turn starts.
    const unErr = on("turn:error", (e) => setLastError(e.message));
    const unListen = on("turn:state", (e) => {
      if (e.phase === "Listening") setLastError(null);
    });
    const unFw = on("firewall:status", (f) =>
      setStatus((s) => (s ? { ...s, firewall: f } : s)),
    );
    const unSwap = [
      on("engine_swap_requested", (e) => setSwaps((s) => applySwap(s, "requested", e))),
      on("engine_swap_done", (e) => setSwaps((s) => applySwap(s, "done", e))),
      on("engine_swap_failed", (e) => setSwaps((s) => applySwap(s, "failed", e))),
    ];
    return () => {
      for (const u of [un, unErr, unListen, unFw, ...unSwap]) void u.then((f) => f());
    };
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
      // A new profile means new STT/TTS engines: load them in the background (no restart).
      // Progress and the result show up below as engine_swap_* events.
      if (needsEngineSwap(patch)) {
        await api.requestEngineSwap("all").catch((e) => setError(swapErrorText(e)));
      }
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

      <label className="block">
        Language model
        <select
          className="ml-2 rounded border px-1"
          value={draft.provider}
          onChange={(e) =>
            setDraft({ ...draft, provider: e.target.value as Settings["provider"] })
          }
        >
          {PROVIDERS.map((p) => (
            <option key={p} value={p}>
              {p === "Ollama" ? "Ollama (local)" : "Anthropic (cloud, asks before sending)"}
            </option>
          ))}
        </select>
      </label>

      <label className="block">
        Anthropic model id
        <input
          className="ml-2 w-64 rounded border px-1 font-mono"
          placeholder="empty = cloud unavailable"
          value={draft.anthropic_model}
          onChange={(e) => setDraft({ ...draft, anthropic_model: e.target.value })}
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
        {(status?.notices ?? []).map((n) => (
          <div key={n} role="status" className="text-amber-700">
            {n}
          </div>
        ))}
        {(["stt", "tts"] as const).map((slot) => {
          const st = swaps[slot];
          return (
            st && (
              <div
                key={slot}
                role={st.state === "failed" ? "alert" : "status"}
                className={st.state === "failed" ? "text-red-600" : "text-neutral-700"}
              >
                {swapText(slot, st)}
                {st.state === "failed" && (
                  <button
                    className="ml-2 rounded border px-2 text-neutral-800"
                    onClick={() =>
                      void api.requestEngineSwap(slot).catch((e) => setError(swapErrorText(e)))
                    }
                  >
                    Retry load
                  </button>
                )}
              </div>
            )
          );
        })}
        {lastError && (
          <div role="alert" className="text-red-600">
            {lastError}
          </div>
        )}
        {status?.settings_error && (
          <div role="alert" className="text-red-600">
            Settings file was unreadable; defaults in use ({status.settings_error})
          </div>
        )}
      </section>

      <ModelsPanel profile={current.profile} />

      <ApiKey
        hasKey={current.has_api_key}
        onChange={(s) => {
          setCurrent(s);
          setDraft((d) => (d ? { ...d, has_api_key: s.has_api_key } : d));
        }}
      />

      <LastRequest />

      {import.meta.env.DEV && <DebugPoint />}
    </main>
  );
}

/** The key goes straight to the OS keychain; this page only ever shows a masked status. */
function ApiKey({ hasKey, onChange }: { hasKey: boolean; onChange: (s: Settings) => void }) {
  const [key, setKey] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const run = (p: Promise<Settings>) =>
    p
      .then((s) => {
        setKey("");
        setMsg(null);
        onChange(s);
      })
      .catch((e) => setMsg(keyErrorText(e)));
  return (
    <section className="space-y-2 border-t pt-3">
      <div className="font-medium">{keyStatusText(hasKey)}</div>
      <input
        type="password"
        autoComplete="off"
        spellCheck={false}
        className="w-64 rounded border px-1"
        placeholder="paste key"
        value={key}
        onChange={(e) => setKey(e.target.value)}
      />
      <button
        className="ml-2 rounded border px-2 disabled:opacity-40"
        disabled={key.trim() === ""}
        onClick={() => void run(api.setApiKey("anthropic", key))}
      >
        Save key
      </button>
      <button
        className="ml-2 rounded border px-2 disabled:opacity-40"
        disabled={!hasKey}
        onClick={() => void run(api.clearApiKey("anthropic"))}
      >
        Clear key
      </button>
      {msg && <div role="alert" className="text-red-600">{msg}</div>}
    </section>
  );
}

/** The most recent turn's request (local or cloud), exactly as prepared. */
function LastRequest() {
  const [last, setLast] = useState<LastRequestDto | null | undefined>(undefined);
  const load = () => void api.getLastRequest().then(setLast).catch(() => setLast(null));
  return (
    <section className="space-y-2 border-t pt-3">
      <div className="flex items-center gap-2">
        <span className="font-medium">Last request</span>
        <button className="rounded border px-2" onClick={load}>
          {last === undefined ? "Show" : "Refresh"}
        </button>
      </div>
      {last === null && <div className="text-neutral-500">No request yet.</div>}
      {last && <RequestFields r={last} cacheKey={last.digest_hex} />}
    </section>
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
