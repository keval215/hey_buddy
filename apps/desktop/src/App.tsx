import { useCallback, useEffect, useState } from "react";
import { api, on } from "./api/client";
import type { Phase } from "./ipc/Phase";
import type { LastRequestDto } from "./ipc/LastRequestDto";
import type { Settings } from "./ipc/Settings";
import type { SettingsIssue } from "./ipc/SettingsIssue";
import type { SettingsPatch } from "./ipc/SettingsPatch";
import { ModelsPanel } from "./models/ModelsPanel";
import { RequestFields } from "./request/RequestView";
import { DenyListPanel } from "./settings/DenyListPanel";
import type { Status } from "./ipc/Status";
import {
  LOCAL_ONLY_OFF_NOTE,
  PROFILES,
  PROVIDERS,
  VISION_MODES,
  buildPatch,
  cloudMissing,
  errorText,
  hotkeyHint,
  hotkeyProblem,
  isEmptyPatch,
  issueText,
  keyErrorText,
  keyStatusText,
  needsEngineSwap,
  phaseText,
  shownVision,
  swapErrorText,
  swapText,
  visionLocked,
  visionNote,
  applySwap,
  type SwapStates,
} from "./settings/form";

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="space-y-2 border-t pt-3">
      <h2 className="font-medium">{title}</h2>
      {children}
    </section>
  );
}

export default function App() {
  const [current, setCurrent] = useState<Settings | null>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [phase, setPhase] = useState<Phase>("Idle");
  const [error, setError] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const [swaps, setSwaps] = useState<SwapStates>({});
  // What the last save did not accept, with the patch it was about (to quote the user's text).
  const [issues, setIssues] = useState<{ list: SettingsIssue[]; patch: SettingsPatch } | null>(
    null,
  );
  const [modelsRefresh, setModelsRefresh] = useState(0);

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
      const res = await api.updateSettings(patch);
      setCurrent(res.settings);
      setDraft(res.settings);
      setError(null);
      setIssues(res.issues.length > 0 ? { list: res.issues, patch } : null);
      if (res.settings.models_dir !== current.models_dir || res.models_dir_changed) {
        setModelsRefresh((n) => n + 1);
      }
      // A new profile or models folder means new STT/TTS engines: load them in the background
      // (no restart). Progress and the result show up below as engine_swap_* events.
      if (needsEngineSwap(patch, res.models_dir_changed)) {
        await api.requestEngineSwap("all").catch((e) => setError(swapErrorText(e)));
      }
    } catch (e) {
      setError(errorText(e));
      setDraft(current);
    }
  }, [current, draft]);

  if (!draft || !current) return <main className="p-4">Loading…</main>;
  const dirty = !isEmptyPatch(buildPatch(current, draft));
  const hkProblem = draft.hotkey.trim() === current.hotkey ? null : hotkeyProblem(draft.hotkey);
  const note = visionNote(draft.profile);
  const missing = cloudMissing(draft);

  return (
    <main className="space-y-4 p-4 text-sm">
      <h1 className="text-xl font-semibold">hey_buddy settings</h1>

      {status?.settings_error && (
        <div role="alert" className="text-red-600">
          {issueText({ field: "file", code: "corrupt" })}
        </div>
      )}
      {(status?.settings_issues ?? []).map((i) => (
        <div key={`${i.field}-${i.code}`} role="alert" className="text-red-600">
          {issueText(i)}
        </div>
      ))}
      {issues?.list.map((i) => (
        <div key={`${i.field}-${i.code}`} role="alert" className="text-red-600">
          {issueText(i, issues.patch)}
        </div>
      ))}

      <Section title="General">
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
          <span className="ml-2 text-neutral-500">
            {draft.profile === "low-memory"
              ? "smaller voice models, no vision"
              : "best quality"}
          </span>
        </label>

        <label className="block">
          Vision (screenshots)
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
          {note && <span className="ml-2 text-neutral-500">{note}</span>}
        </label>

        <label className="block">
          Push-to-talk hotkey
          <input
            className="ml-2 w-48 rounded border px-1"
            value={draft.hotkey}
            aria-invalid={hkProblem !== null}
            onChange={(e) => setDraft({ ...draft, hotkey: e.target.value })}
          />
          {hkProblem && (
            <span role="status" className="ml-2 text-amber-700">
              {hotkeyHint(hkProblem)}
            </span>
          )}
        </label>
        <div className="text-neutral-500">
          Must include Ctrl, Alt or Super, for example Ctrl+Alt+Space. The microphone is only
          open while you hold it.
        </div>
      </Section>

      <Section title="Models folder">
        <label className="block">
          Folder
          <input
            className="ml-2 w-96 rounded border px-1 font-mono"
            placeholder={draft.default_models_dir || "default"}
            value={draft.models_dir}
            onChange={(e) => setDraft({ ...draft, models_dir: e.target.value })}
          />
        </label>
        <div className="text-neutral-500">
          Empty uses the default: {draft.default_models_dir || "(not known yet)"}. Use a full
          path to an existing folder (or one that can be created). Network paths, system
          folders and paths with '..' are not accepted. Changing it re-reads the models list and
          reloads the voice engines from the new folder.
        </div>
      </Section>

      <Section title="Language model and privacy">
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={draft.local_only}
            onChange={(e) => setDraft({ ...draft, local_only: e.target.checked })}
          />
          Local only (never call cloud models)
        </label>
        {!draft.local_only && (
          <div role="note" className="text-amber-700">
            {LOCAL_ONLY_OFF_NOTE}
          </div>
        )}

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
        <div className="text-neutral-500">
          {draft.provider === "Anthropic" && missing.length > 0
            ? `Cloud is not ready yet: ${missing.join(", ")}.`
            : "Cloud needs Anthropic chosen, Local only off, a model id and a stored key, and every request is previewed first."}
        </div>
      </Section>

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

      <ApiKey
        hasKey={current.has_api_key}
        onChange={(s) => {
          setCurrent(s);
          setDraft((d) => (d ? { ...d, has_api_key: s.has_api_key } : d));
        }}
      />

      <DenyListPanel />

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
      </section>

      <ModelsPanel profile={current.profile} refreshKey={modelsRefresh} />

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
