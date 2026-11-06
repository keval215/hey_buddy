import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { api } from "../api/client";
import type { DenyListDto } from "../ipc/DenyListDto";
import type { Settings } from "../ipc/Settings";
import type { SettingsResult } from "../ipc/SettingsResult";
import type { WizardEvent } from "../ipc/WizardEvent";
import type { WizardView } from "../ipc/WizardView";
import { ModelsPanel } from "../models/ModelsPanel";
import { PROFILES, hotkeyHint, hotkeyProblem, needsEngineSwap } from "../settings/form";
import { WizardController, type ControllerState } from "./controller";
import {
  NO_SOURCE_NOTE,
  OLLAMA_INSTALL_TEXT,
  PRIVACY_POINTS,
  STEPS,
  canBack,
  canSkip,
  hotkeyAccepted,
  machineLines,
  meterPercent,
  micText,
  nextLabel,
  ollamaLines,
  recommendationText,
  safetyIntro,
  safetyRows,
  skipNote,
  stepNumber,
  stepTitle,
} from "./logic";

/** Handlers that reach outside the wizard (settings window plumbing). */
export interface Actions {
  send(e: WizardEvent): void;
  checkMachine(): void;
  checkOllama(): void;
  testMic(): void;
  /** Save the chosen profile through the normal settings flow, then reload the wizard. */
  useProfile(profile: string): void;
  /** Apply a hotkey through the normal settings flow and report whether it took effect. */
  applyHotkey(text: string): void;
}

/** Pure view of the wizard: everything it shows comes in through props. */
export function WizardViewPanel({
  ctl,
  settings,
  denyCount,
  hotkeyMessage,
  actions,
  modelsRefresh = 0,
}: {
  ctl: ControllerState;
  settings: Settings;
  denyCount: number | null;
  hotkeyMessage: string | null;
  actions: Actions;
  modelsRefresh?: number;
}) {
  const view = ctl.view;
  const headingRef = useRef<HTMLHeadingElement>(null);
  const step = view?.state.step;
  // Keyboard users land on the new step's heading when the step changes.
  useEffect(() => headingRef.current?.focus(), [step]);

  if (!view) {
    return (
      <main className="p-4 text-sm" aria-busy="true">
        {ctl.error ? <div role="alert" className="text-red-600">{ctl.error}</div> : "Loading…"}
      </main>
    );
  }
  const st = view.state;
  const skippable = canSkip(st.step);
  const noteId = "wizard-skip-note";
  return (
    <main className="space-y-4 p-4 text-sm">
      <h1 className="text-xl font-semibold">Set up hey_buddy</h1>
      <nav aria-label="Setup steps">
        <ol className="flex flex-wrap gap-2">
          {STEPS.map((s) => (
            <li
              key={s.step}
              aria-current={s.step === st.step ? "step" : undefined}
              className={
                s.step === st.step
                  ? "rounded border border-neutral-800 px-2 py-0.5 font-medium"
                  : "rounded border px-2 py-0.5 text-neutral-500"
              }
            >
              {stepNumber(s.step)}. {s.title}
              {st.skipped_steps.includes(s.step) ? " (skipped)" : ""}
            </li>
          ))}
        </ol>
      </nav>

      <section className="space-y-2 border-t pt-3" aria-labelledby="wizard-heading">
        <h2 id="wizard-heading" tabIndex={-1} ref={headingRef} className="font-medium">
          {stepNumber(st.step)}. {stepTitle(st.step)}
        </h2>
        {ctl.error && (
          <div role="alert" className="text-red-600">
            {ctl.error}
          </div>
        )}
        <StepBody
          view={view}
          ctl={ctl}
          settings={settings}
          denyCount={denyCount}
          hotkeyMessage={hotkeyMessage}
          actions={actions}
          modelsRefresh={modelsRefresh}
        />
      </section>

      <div className="flex flex-wrap items-center gap-2 border-t pt-3">
        <button
          type="button"
          className="rounded border px-3 py-1 disabled:opacity-40"
          disabled={!canBack(st) || ctl.busy}
          onClick={() => actions.send("Back")}
        >
          Back
        </button>
        {st.step !== "Done" && (
          <>
            <button
              type="button"
              className="rounded border px-3 py-1 disabled:opacity-40"
              disabled={!skippable || ctl.busy}
              aria-describedby={noteId}
              onClick={() => actions.send("Skip")}
            >
              Skip
            </button>
            <span id={noteId} className="text-neutral-500">
              {skipNote(st.step)}
            </span>
          </>
        )}
        <button
          type="button"
          className="ml-auto rounded border border-neutral-800 px-3 py-1 font-medium disabled:opacity-40"
          disabled={ctl.busy}
          onClick={() => actions.send(st.step === "Done" ? "Finish" : "Next")}
        >
          {nextLabel(st.step)}
        </button>
      </div>
    </main>
  );
}

function StepBody(p: {
  view: WizardView;
  ctl: ControllerState;
  settings: Settings;
  denyCount: number | null;
  hotkeyMessage: string | null;
  actions: Actions;
  modelsRefresh: number;
}) {
  const { view, ctl, settings, actions } = p;
  const st = view.state;
  switch (st.step) {
    case "Welcome":
      return (
        <div className="space-y-2">
          <p>hey_buddy sits next to your cursor and talks with you. What it does with your data:</p>
          <ul className="list-disc space-y-1 pl-5">
            {PRIVACY_POINTS.map((t) => (
              <li key={t}>{t}</li>
            ))}
          </ul>
        </div>
      );
    case "MachineCheck":
      return (
        <div className="space-y-2">
          <p>
            Reading your memory and graphics card tells us which set of models fits. Nothing is
            sent anywhere.
          </p>
          <button
            type="button"
            className="rounded border px-3 py-1 disabled:opacity-40"
            disabled={ctl.busy}
            onClick={actions.checkMachine}
          >
            {st.machine ? "Check again" : "Check my computer"}
          </button>
          {st.machine && (
            <div className="space-y-1">
              <ul className="list-disc pl-5">
                {machineLines(st.machine).map((l) => (
                  <li key={l}>{l}</li>
                ))}
              </ul>
              <p>{recommendationText(st, view.profile)}</p>
              <ProfilePicker
                current={view.profile}
                recommended={st.recommended_profile}
                busy={ctl.busy}
                onUse={actions.useProfile}
              />
            </div>
          )}
        </div>
      );
    case "Microphone":
      return (
        <div className="space-y-2">
          <p>
            The test opens the microphone for a moment and shows how loud it is. Nothing is
            recorded or saved. The buddy only ever listens while you hold its hotkey.
          </p>
          <button
            type="button"
            className="rounded border px-3 py-1 disabled:opacity-40"
            disabled={ctl.busy}
            onClick={actions.testMic}
          >
            {ctl.busy ? "Listening…" : "Test microphone"}
          </button>
          <p className="text-neutral-500">Say something while it listens (up to 5 seconds).</p>
          {ctl.mic && (
            <div className="space-y-1">
              {ctl.mic.error === null && (
                <div aria-label="microphone level">
                  <progress className="w-48" max={100} value={meterPercent(ctl.mic.peak)} />
                  <span className="ml-2">level {meterPercent(ctl.mic.peak)}%</span>
                </div>
              )}
              <p role="status">{micText(ctl.mic)}</p>
            </div>
          )}
          {!ctl.mic && st.mic_ok !== null && (
            <p role="status">
              {st.mic_ok ? "Last test: sound was heard." : "Last test: no sound was heard."}
            </p>
          )}
        </div>
      );
    case "Ollama":
      return (
        <div className="space-y-2">
          <p>
            The buddy asks a language model running on your computer. This checks, once, whether
            Ollama answers on this computer. Nothing is installed or started.
          </p>
          <button
            type="button"
            className="rounded border px-3 py-1 disabled:opacity-40"
            disabled={ctl.busy}
            onClick={actions.checkOllama}
          >
            {st.ollama ? "Check again" : "Check"}
          </button>
          {st.ollama && (
            <ul className="list-disc pl-5" role="status">
              {ollamaLines(st.ollama).map((l) => (
                <li key={l}>{l}</li>
              ))}
            </ul>
          )}
          {(!st.ollama || !st.ollama.running) && (
            <div className="space-y-1 text-neutral-600">
              {OLLAMA_INSTALL_TEXT.map((l) => (
                <p key={l}>{l}</p>
              ))}
            </div>
          )}
          <p className="text-neutral-500">
            Models to look for: {view.ollama_tags.join(", ") || "none"}.
          </p>
        </div>
      );
    case "Models":
      return (
        <div className="space-y-2">
          <p>
            Models for the <b>{view.profile}</b> profile. Nothing downloads until you click
            Download, and every file is checked against its checksum first.
          </p>
          <p className="text-neutral-500">{NO_SOURCE_NOTE}</p>
          <ModelsPanel profile={view.profile} onlyIds={view.model_ids} refreshKey={p.modelsRefresh} />
        </div>
      );
    case "Hotkey":
      return (
        <HotkeyStep
          current={settings.hotkey}
          result={st.hotkey_ok}
          message={p.hotkeyMessage}
          busy={ctl.busy}
          onApply={actions.applyHotkey}
        />
      );
    case "SafetyDefaults": {
      const rows = safetyRows(settings);
      return (
        <div className="space-y-2">
          <p>{safetyIntro(rows)}</p>
          <ul className="space-y-1">
            {rows.map((r) => (
              <li key={r.label}>
                {r.label}: <b>{r.now}</b>
                {!r.safe && <span className="ml-2 text-amber-700">(will be switched to the safe value)</span>}
              </li>
            ))}
            <li>
              Apps the buddy never looks at:{" "}
              <b>{p.denyCount === null ? "built-in list in use" : `${p.denyCount} entries`}</b> (change
              it later in Settings)
            </li>
          </ul>
        </div>
      );
    }
    case "Done":
      return (
        <div className="space-y-2">
          <p>You are set up. Hold your hotkey ({settings.hotkey}) and talk.</p>
          {st.skipped_steps.length > 0 && (
            <p className="text-neutral-600">
              You skipped: {st.skipped_steps.map(stepTitle).join(", ")}. You can run setup again
              from Settings at any time.
            </p>
          )}
        </div>
      );
  }
}

function ProfilePicker({
  current,
  recommended,
  busy,
  onUse,
}: {
  current: string;
  recommended: string | null;
  busy: boolean;
  onUse: (profile: string) => void;
}) {
  const [choice, setChoice] = useState<string>(recommended ?? current);
  return (
    <div className="space-x-2">
      <label>
        Profile
        <select
          className="ml-2 rounded border px-1"
          value={choice}
          onChange={(e) => setChoice(e.target.value)}
        >
          {PROFILES.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </label>
      <button
        type="button"
        className="rounded border px-2 disabled:opacity-40"
        disabled={busy || choice === current}
        onClick={() => onUse(choice)}
      >
        Use this profile
      </button>
    </div>
  );
}

function HotkeyStep({
  current,
  result,
  message,
  busy,
  onApply,
}: {
  current: string;
  result: boolean | null;
  message: string | null;
  busy: boolean;
  onApply: (text: string) => void;
}) {
  const [text, setText] = useState(current);
  const problem = text.trim() === current ? null : hotkeyProblem(text);
  return (
    <div className="space-y-2">
      <p>
        Hold this shortcut to talk. It must include Ctrl, Alt or Super, for example
        Ctrl+Alt+Space.
      </p>
      <label className="block">
        Push-to-talk hotkey
        <input
          className="ml-2 w-48 rounded border px-1"
          value={text}
          aria-invalid={problem !== null}
          onChange={(e) => setText(e.target.value)}
        />
      </label>
      {problem && (
        <p role="status" className="text-amber-700">
          {hotkeyHint(problem)}
        </p>
      )}
      <button
        type="button"
        className="rounded border px-3 py-1 disabled:opacity-40"
        disabled={busy || problem !== null || text.trim() === ""}
        onClick={() => onApply(text)}
      >
        Save and test this hotkey
      </button>
      {message && <p role="status">{message}</p>}
      {!message && result !== null && (
        <p role="status">{result ? "The hotkey is ready." : "The hotkey could not be registered."}</p>
      )}
    </div>
  );
}

/**
 * The wizard in the settings window. Mounting reads the saved progress and nothing else; every
 * check (computer, microphone, Ollama) and every download is started by a click on its button.
 */
export function Wizard({
  settings,
  onSettings,
  onClose,
}: {
  settings: Settings;
  onSettings: (r: SettingsResult) => void;
  onClose: () => void;
}) {
  const [ctl] = useState(() => new WizardController(realApi));
  const state = useSyncExternalStore(ctl.subscribe, ctl.getState, ctl.getState);
  const [deny, setDeny] = useState<DenyListDto | null>(null);
  const [hotkeyMessage, setHotkeyMessage] = useState<string | null>(null);
  const [modelsRefresh, setModelsRefresh] = useState(0);

  // Reads the saved progress only.
  useEffect(() => void ctl.load(), [ctl]);

  const step = state.view?.state.step;
  useEffect(() => {
    // Local read of the deny list for the safety step.
    if (step === "SafetyDefaults") void api.getDenyList().then(setDeny).catch(() => setDeny(null));
  }, [step]);

  const actions: Actions = {
    send: (e) => {
      setHotkeyMessage(null);
      void ctl.send(e).then(() => {
        if (e === "Finish" && !ctl.getState().error) onClose();
      });
    },
    checkMachine: () => void ctl.checkMachine(),
    checkOllama: () => void ctl.checkOllama(),
    testMic: () => void ctl.testMic(),
    useProfile: (profile) => {
      void api
        .updateSettings({ profile })
        .then(async (res) => {
          onSettings(res);
          setModelsRefresh((n) => n + 1);
          if (needsEngineSwap({ profile })) await api.requestEngineSwap("all").catch(() => undefined);
          await ctl.load();
        })
        .catch(() => undefined);
    },
    applyHotkey: (text) => {
      void (async () => {
        try {
          const res = await api.updateSettings({ hotkey: text });
          onSettings(res);
          const ok = hotkeyAccepted(res, await api.getStatus());
          setHotkeyMessage(
            ok
              ? `Saved: ${res.settings.hotkey}. It is registered and ready.`
              : "That hotkey could not be used (invalid, or another app owns it). The previous one is still active.",
          );
          await ctl.send({ HotkeyResult: ok });
        } catch {
          setHotkeyMessage("The hotkey could not be saved.");
        }
      })();
    },
  };

  return (
    <WizardViewPanel
      ctl={state}
      settings={settings}
      denyCount={deny ? deny.entries.length : null}
      hotkeyMessage={hotkeyMessage}
      actions={actions}
      modelsRefresh={modelsRefresh}
    />
  );
}

const realApi = {
  state: () => api.wizardState(),
  event: (e: WizardEvent) => api.wizardEvent(e),
  probeMachine: () => api.wizardProbeMachine(),
  probeMic: () => api.wizardProbeMic(),
  probeOllama: () => api.wizardProbeOllama(),
};
