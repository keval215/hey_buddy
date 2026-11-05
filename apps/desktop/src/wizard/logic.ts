// Pure helpers of the first-run wizard (unit-tested). No Tauri calls and no network in here:
// the core probes, this only words things and decides which buttons are enabled.
import type { MachineInfo } from "../ipc/MachineInfo";
import type { MicProbeDto } from "../ipc/MicProbeDto";
import type { ModelRow } from "../ipc/ModelRow";
import type { OllamaInfo } from "../ipc/OllamaInfo";
import type { Settings } from "../ipc/Settings";
import type { SettingsResult } from "../ipc/SettingsResult";
import type { Status } from "../ipc/Status";
import type { WizardProfile } from "../ipc/WizardProfile";
import type { WizardState } from "../ipc/WizardState";
import type { WizardStep } from "../ipc/WizardStep";

export const STEPS: { step: WizardStep; title: string }[] = [
  { step: "Welcome", title: "Welcome" },
  { step: "MachineCheck", title: "Your computer" },
  { step: "Microphone", title: "Microphone" },
  { step: "Ollama", title: "Ollama" },
  { step: "Models", title: "Models" },
  { step: "Hotkey", title: "Hotkey" },
  { step: "SafetyDefaults", title: "Safety" },
  { step: "Done", title: "Done" },
];

export function stepNumber(step: WizardStep): number {
  return STEPS.findIndex((s) => s.step === step) + 1;
}

export function stepTitle(step: WizardStep): string {
  return STEPS.find((s) => s.step === step)?.title ?? "";
}

/** What the buddy does with your data, before anything else. */
export const PRIVACY_POINTS: string[] = [
  "Local first: speech recognition, the spoken reply, the privacy checks and the logs all run on this computer.",
  "The buddy only looks at your screen when you press the hotkey, and never at password fields or apps on the deny list.",
  "Cloud models are off by default. If you ever switch one on, nothing is sent until you have seen exactly what would be sent and clicked Send.",
  "The agent that can operate apps is off, and every action it wants to take needs your approval.",
  "Nothing in this setup downloads anything, listens to the microphone or contacts anything until you click that step's button.",
];

export function canSkip(step: WizardStep): boolean {
  return step !== "SafetyDefaults" && step !== "Done";
}

/** Shown next to Skip. Skipping never changes a setting, and the safe defaults stay on. */
export function skipNote(step: WizardStep): string {
  return canSkip(step)
    ? "Skipping is fine: your safe settings stay on."
    : step === "SafetyDefaults"
      ? "This step cannot be skipped. Your safe settings stay on either way."
      : "";
}

export function canBack(state: WizardState): boolean {
  return state.step !== "Welcome" && !state.completed;
}

export function nextLabel(step: WizardStep): string {
  switch (step) {
    case "Welcome":
      return "Start";
    case "SafetyDefaults":
      return "Keep these safe settings";
    case "Done":
      return "Finish";
    default:
      return "Next";
  }
}

/** Lines for the machine result, in plain text. */
export function machineLines(m: MachineInfo): string[] {
  const gb = (mb: number) => `${(mb / 1024).toFixed(mb >= 10 * 1024 ? 0 : 1)} GB`;
  return [
    m.ram_mb === null ? "Memory (RAM): could not be read" : `Memory (RAM): ${gb(m.ram_mb)}`,
    m.gpu_name === null ? "Graphics card: not found" : `Graphics card: ${m.gpu_name}`,
    m.vram_mb === null ? "Graphics memory: unknown" : `Graphics memory: ${gb(m.vram_mb)}`,
  ];
}

export function profileLabel(p: WizardProfile): string {
  return p === "default" ? "default (best quality)" : "low-memory (smaller models, no screen vision)";
}

export function recommendationText(state: WizardState, current: string): string | null {
  const rec = state.recommended_profile;
  if (rec === null) return null;
  const base = `Recommended profile: ${profileLabel(rec)}.`;
  if (state.machine?.ram_mb === null || state.machine?.vram_mb === null) {
    return `${base} Some numbers could not be read, so the smaller choice is recommended.`;
  }
  return current === rec ? `${base} It is already selected.` : `${base} You are using ${current}.`;
}

export const OLLAMA_INSTALL_TEXT = [
  "Ollama runs the language model on your computer. This app never installs or starts it for you.",
  "To install it yourself, get it from ollama.com (type the address in your browser; nothing here opens it).",
  "After installing, start Ollama, then pull the models below with: ollama pull <model>",
];

/** Result lines of the Ollama check. "Not running" covers not installed and not started. */
export function ollamaLines(o: OllamaInfo): string[] {
  if (!o.running) {
    return [
      "Ollama did not answer on this computer (port 11434). It may not be installed, or it is not started.",
    ];
  }
  return [
    "Ollama is running.",
    ...o.models.map((m) =>
      m.present ? `Model ${m.tag}: installed` : `Model ${m.tag}: not installed yet (ollama pull ${m.tag})`,
    ),
  ];
}

export function micText(r: MicProbeDto): string {
  if (r.error !== null) return micErrorText(r.error);
  return r.view.state.mic_ok === true
    ? "Your microphone works: sound was heard."
    : "The microphone opened but almost no sound was heard. Check that the right microphone is selected and not muted, then try again.";
}

export function micErrorText(code: string): string {
  switch (code) {
    case "mic_unavailable":
      return "The microphone test is not available in this build (voice engines are not built in).";
    case "mic_failed":
      return "The microphone could not be opened. Check that it is plugged in and allowed for this app.";
    case "mic_timeout":
      return "The microphone did not answer within 5 seconds.";
    case "busy":
      return "The buddy is in the middle of a conversation. Try again in a moment.";
    default:
      return "The microphone test failed.";
  }
}

/** The level meter: 0 to 100 from a 0..1 peak. */
export function meterPercent(peak: number): number {
  if (!Number.isFinite(peak)) return 0;
  return Math.round(Math.min(1, Math.max(0, peak)) * 100);
}

export function wizardErrorText(code: unknown): string {
  switch (code) {
    case "wrong_step":
      return "That check belongs to a different step.";
    case "forbidden_event":
      return "That action is not allowed.";
    case "io_error":
      return "Your progress could not be saved.";
    case "busy":
      return "The buddy is busy. Try again in a moment.";
    case "probe_failed":
      return "The check failed.";
    default:
      return typeof code === "string" && code.length > 0 ? code : "Something went wrong";
  }
}

/** The models the wizard lists: the profile's own, in registry order. */
export function modelsForProfile(rows: ModelRow[], ids: string[]): ModelRow[] {
  return rows.filter((r) => ids.includes(r.id));
}

export const NO_SOURCE_NOTE =
  "A model marked \"no download source yet\" has no verified download address in this version, so it cannot be downloaded here. Nothing is guessed or fetched from elsewhere; you can place the file in the models folder yourself (see docs/models.md in the project).";

/** The hotkey step passes when the core accepted the chord and the OS registered it. */
export function hotkeyAccepted(res: SettingsResult, status: Status): boolean {
  return res.issues.every((i) => i.field !== "hotkey") && status.hotkey_registered;
}

export type SafetyRow = { label: string; now: string; safe: boolean };

function visionNow(s: Settings): string {
  if (s.profile === "low-memory") return "off (this profile has no vision model)";
  switch (s.vision_mode) {
    case "Off":
      return "off";
    case "Auto":
      return "auto (only when a question needs the screen)";
    default:
      return "always (will be switched to auto)";
  }
}

/** What the safety step shows, from the real current settings. `safe` = already the safe value. */
export function safetyRows(s: Settings): SafetyRow[] {
  const row = (label: string, on: boolean, wantOn: boolean, onText = "on", offText = "off") => ({
    label,
    now: on ? onText : offText,
    safe: on === wantOn,
  });
  return [
    row("Local only (nothing leaves this computer)", s.local_only, true),
    row("Observe mode (agent tasks start as a dry run)", s.observe_mode, true),
    row("Agent (operating apps)", s.agent_enabled, false),
    row("Agent mode for voice requests", s.agent_mode, false),
    {
      label: "Screen vision",
      now: visionNow(s),
      safe: s.profile === "low-memory" || s.vision_mode !== "Always",
    },
  ];
}

export function safetyChanges(rows: SafetyRow[]): number {
  return rows.filter((r) => !r.safe).length;
}

export function safetyIntro(rows: SafetyRow[]): string {
  const n = safetyChanges(rows);
  return n === 0
    ? "Your settings are already the safe ones. Confirming keeps them. This setup can only turn protections on, never off."
    : `${n} of these are not at the safe value. Confirming switches them to the safe value. This setup can only turn protections on, never off.`;
}

/** The wizard opens by itself until it has been finished once. */
export function shouldAutoShow(state: WizardState | null): boolean {
  return state !== null && !state.completed;
}
