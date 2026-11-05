// Pure settings-form logic (unit-tested).
import type { AgentStatusDto } from "../ipc/AgentStatusDto";
import type { AgentTaskDto } from "../ipc/AgentTaskDto";
import type { EngineSlotName } from "../ipc/EngineSlotName";
import type { EngineSwapEvent } from "../ipc/EngineSwapEvent";
import type { IssueCode } from "../ipc/IssueCode";
import type { SettingsIssue } from "../ipc/SettingsIssue";
import type { Phase } from "../ipc/Phase";
import type { ProviderChoice } from "../ipc/ProviderChoice";
import type { Settings } from "../ipc/Settings";
import type { SettingsPatch } from "../ipc/SettingsPatch";
import type { VisionMode } from "../ipc/VisionMode";

export const PROFILES = ["default", "low-memory"] as const;
export const VISION_MODES: VisionMode[] = ["Off", "Auto", "Always"];
export const PROVIDERS: ProviderChoice[] = ["Ollama", "Anthropic"];

/** `low-memory` has no vision model: the core forces vision Off there. */
export function visionLocked(profile: string): boolean {
  return profile === "low-memory";
}

/** Value shown in the vision select for a draft. */
export function shownVision(draft: Settings): VisionMode {
  return visionLocked(draft.profile) ? "Off" : draft.vision_mode;
}

/** Only the fields that differ from `current`; empty when nothing changed. */
export function buildPatch(current: Settings, draft: Settings): SettingsPatch {
  const patch: SettingsPatch = {};
  if (draft.local_only !== current.local_only) patch.local_only = draft.local_only;
  if (draft.profile !== current.profile) patch.profile = draft.profile;
  if (draft.provider !== current.provider) patch.provider = draft.provider;
  if (!visionLocked(draft.profile) && draft.vision_mode !== current.vision_mode) {
    patch.vision_mode = draft.vision_mode;
  }
  const model = draft.anthropic_model.trim();
  if (model !== current.anthropic_model) patch.anthropic_model = model;
  const hk = draft.hotkey.trim();
  if (hk !== current.hotkey) patch.hotkey = hk;
  const kk = draft.kill_hotkey.trim();
  if (kk !== current.kill_hotkey) patch.kill_hotkey = kk;
  const dir = draft.models_dir.trim();
  if (dir !== current.models_dir) patch.models_dir = dir;
  if (draft.observe_mode !== current.observe_mode) patch.observe_mode = draft.observe_mode;
  if (draft.agent_enabled !== current.agent_enabled) patch.agent_enabled = draft.agent_enabled;
  // Voice routing needs the agent: turning the agent off turns the routing off with it.
  const mode = draft.agent_enabled && draft.agent_mode;
  if (mode !== current.agent_mode) patch.agent_mode = mode;
  if (draft.diagnostic_logs !== current.diagnostic_logs) {
    patch.diagnostic_logs = draft.diagnostic_logs;
  }
  return patch;
}

export function isEmptyPatch(p: SettingsPatch): boolean {
  return Object.keys(p).length === 0;
}

export function phaseText(p: Phase): string {
  return p === "Idle" ? "Idle" : p;
}

/** Errors from the core are fixed text; show them as-is, with a fallback. */
export function errorText(e: unknown): string {
  if (typeof e === "string" && e.length > 0) return e;
  return "Something went wrong";
}

/** Fixed core tokens for key errors -> user text. */
export function keyErrorText(e: unknown): string {
  switch (e) {
    case "credential_store_unsupported":
      return "Key storage is not available on this platform yet";
    case "invalid_key":
      return "That does not look like a valid key";
    case "unsupported_provider":
      return "Unknown provider";
    case "keychain_error":
      return "The OS keychain refused the request";
    default:
      return errorText(e);
  }
}

/** Shown instead of the key: never the key itself. */
export function keyStatusText(hasKey: boolean): string {
  return hasKey ? "Anthropic API key: stored in the OS keychain (hidden)" : "Anthropic API key: not set";
}

export type SwapState = {
  state: "loading" | "done" | "failed";
  id: string;
  error: string | null;
  path: string | null;
};
export type SwapStates = Partial<Record<EngineSlotName, SwapState>>;
export type SwapKind = "requested" | "done" | "failed";

/**
 * The profile decides the engines, and so does the models folder (`modelsDirChanged` comes from
 * the core's answer, which knows whether the folder really changed after validation).
 */
export function needsEngineSwap(patch: SettingsPatch, modelsDirChanged = false): boolean {
  return patch.profile !== undefined || modelsDirChanged;
}

/** Fold one engine_swap_* event into the per-slot status. */
export function applySwap(states: SwapStates, kind: SwapKind, ev: EngineSwapEvent): SwapStates {
  const state = kind === "requested" ? "loading" : kind;
  return {
    ...states,
    [ev.slot]: { state, id: ev.engine_id, error: ev.error, path: ev.path },
  };
}

const SLOT_NAMES: Record<EngineSlotName, string> = {
  stt: "Speech recognition",
  tts: "Speech synthesis",
};

/** One status line per slot; failures name the model id and, for a missing file, where it goes. */
export function swapText(slot: EngineSlotName, s: SwapState): string {
  const name = SLOT_NAMES[slot];
  switch (s.state) {
    case "loading":
      return `${name}: loading ${s.id}…`;
    case "done":
      return `${name}: ${s.id} ready`;
    case "failed":
      return s.error === "model_missing" && s.path
        ? `${name}: ${s.id} could not load, the model file is missing. Expected at ${s.path}`
        : `${name}: ${s.id} could not load (${s.error ?? "load_failed"})`;
  }
}

/** Fixed core tokens from request_engine_swap -> user text. */
export function swapErrorText(e: unknown): string {
  switch (e) {
    case "loader_unavailable":
      return "Voice engines are not built into this version, so nothing was loaded";
    case "invalid_model":
      return "That profile has no valid voice models";
    default:
      return errorText(e);
  }
}

// ---------------------------------------------------------------- S2-7: validation and issues
// The core validates everything; these mirrors only give quick feedback while typing and turn
// the core's typed issues into plain sentences.

export const DEFAULT_HOTKEY = "Ctrl+Alt+Space";
export const DEFAULT_KILL_HOTKEY = "Ctrl+Alt+Shift+K";

/** Explanation shown when `local_only` is switched off. */
export const LOCAL_ONLY_OFF_NOTE =
  "Cloud models are allowed now, but nothing is sent on its own: every cloud request first shows you exactly what would be sent, and only your Send click sends it.";

/** Why vision is locked, or null when it can be changed. */
export function visionNote(profile: string): string | null {
  return visionLocked(profile)
    ? "The low-memory profile has no vision model, so screenshots are never used (vision is Off)."
    : null;
}

/** Quick check of a hotkey as typed: the issue code the core would give, or null. */
export function hotkeyProblem(text: string): IssueCode | null {
  const t = text.trim();
  if (t.length > 64) return "too_long";
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f\u061c\u200b-\u200f\u2028-\u202e\u2060-\u2069\ufeff]/.test(t)) {
    return "control_chars";
  }
  const parts = t.split("+").map((p) => p.trim().toLowerCase());
  const mods = new Set<string>();
  const alias: Record<string, string> = {
    ctrl: "ctrl",
    control: "ctrl",
    alt: "alt",
    option: "alt",
    shift: "shift",
    super: "super",
    cmd: "super",
    command: "super",
    meta: "super",
    win: "super",
  };
  let keys = 0;
  for (const p of parts) {
    const m = alias[p];
    if (m) {
      if (mods.has(m)) return "invalid_format";
      mods.add(m);
    } else if (/^([a-z0-9]|space|f([1-9]|1[0-9]|2[0-4]))$/.test(p)) {
      keys += 1;
    } else {
      return "invalid_format";
    }
  }
  if (keys !== 1) return "invalid_format";
  if (!(mods.has("ctrl") || mods.has("alt") || mods.has("super"))) return "needs_modifier";
  return null;
}

/**
 * Quick check of the kill-switch hotkey as typed: the push-to-talk rules plus at least two
 * modifiers and not the push-to-talk chord. The core decides; this only gives early feedback.
 */
export function killHotkeyProblem(text: string, ptt: string): IssueCode | null {
  const base = hotkeyProblem(text);
  if (base) return base;
  const parts = text.split("+").map((p) => p.trim().toLowerCase());
  const mods = new Set(
    parts
      .map((p) => ({ control: "ctrl", option: "alt", cmd: "super", command: "super", meta: "super", win: "super" })[p as never] ?? p)
      .filter((p) => ["ctrl", "alt", "shift", "super"].includes(p)),
  );
  if (mods.size < 2) return "needs_two_modifiers";
  const canon = (t: string) =>
    t
      .split("+")
      .map((p) => p.trim().toLowerCase())
      .sort()
      .join("+");
  return canon(text) === canon(ptt) ? "duplicate" : null;
}

/** Text for the user's own input in a message: no control or direction-changing characters. */
export function shownValue(s: string, max = 40): string {
  const clean = s.replace(
    // eslint-disable-next-line no-control-regex
    /[\u0000-\u001f\u007f-\u009f\u061c\u200b-\u200f\u2028-\u202e\u2060-\u2069\ufeff]/g,
    "",
  );
  return clean.length > max ? `${clean.slice(0, max)}…` : clean;
}

const REASONS: Record<IssueCode, string> = {
  unknown: "that is not one of the choices",
  needs_modifier: "needs Ctrl/Alt/Super",
  needs_two_modifiers: "the stop shortcut needs at least two modifiers (for example Ctrl+Alt+Shift)",
  duplicate: "it is the same as the other shortcut",
  invalid_format: "that is not in a form that is accepted",
  too_long: "it is too long",
  control_chars: "it contains hidden or control characters",
  not_absolute: "it must be a full path (starting with / or a drive letter)",
  traversal: "it must not contain '..'",
  network_path: "network and device paths are not allowed",
  device_name: "it uses a reserved device name",
  system_dir: "it is a system folder or a drive root",
  not_a_directory: "it is not a folder",
  not_creatable: "the folder cannot be created there",
  symlink: "it is a shortcut (symbolic link) to another place",
  unavailable: "another app is already using that shortcut",
  corrupt: "the settings file could not be read",
};

export function issueReason(code: IssueCode): string {
  return REASONS[code];
}

/** Fixed hotkey hint for the form. */
export function hotkeyHint(code: IssueCode): string {
  return code === "invalid_format"
    ? "Use Ctrl, Alt or Super plus one key, for example Ctrl+Alt+Space."
    : `Not valid: ${issueReason(code)}.`;
}

/**
 * One sentence per issue, naming what was kept. `attempted` is what the user asked for in this
 * save (their own text, cleaned for display); the core never returns it.
 */
export function issueText(issue: SettingsIssue, attempted?: SettingsPatch): string {
  const why = issueReason(issue.code);
  switch (issue.field) {
    case "hotkey": {
      const x = attempted?.hotkey;
      const head = x !== undefined ? `Your hotkey '${shownValue(x)}'` : "The saved hotkey";
      return `${head} was not saved: ${why}. Using the default (${DEFAULT_HOTKEY}).`;
    }
    case "kill_hotkey": {
      const x = attempted?.kill_hotkey;
      const head = x !== undefined ? `Your stop shortcut '${shownValue(x)}'` : "The saved stop shortcut";
      if (issue.code === "unavailable") {
        return `${head} was not saved: the chord could not be registered; the previous one is still active.`;
      }
      return `${head} was not saved: ${why}. Using the default (${DEFAULT_KILL_HOTKEY}).`;
    }
    case "models_dir": {
      const x = attempted?.models_dir;
      const head = x !== undefined ? `Your models folder '${shownValue(x)}'` : "The saved models folder";
      return `${head} was not saved: ${why}. Using the default folder.`;
    }
    case "anthropic_model": {
      const x = attempted?.anthropic_model;
      const head = x !== undefined ? `Your model id '${shownValue(x)}'` : "The saved model id";
      return `${head} was not saved: ${why}. Cloud stays unavailable until a valid model id is set.`;
    }
    case "profile":
      return "That profile is not known, so the default profile is in use.";
    case "file":
      return "The settings file could not be read, so the default settings are in use.";
  }
}

/** What is still missing before a cloud request could even be previewed. */
export function cloudMissing(s: Settings): string[] {
  const out: string[] = [];
  if (s.provider !== "Anthropic") out.push("choose Anthropic as the language model");
  if (s.local_only) out.push("turn off Local only");
  if (s.anthropic_model.trim() === "") out.push("set a model id");
  if (!s.has_api_key) out.push("store an API key");
  return out;
}

/** Explains observe mode: a dry run first, then a separate question. */
export const OBSERVE_MODE_NOTE =
  "Observe mode (on by default) means every agent task starts as a dry run: it shows what it " +
  "WOULD do and does nothing. Then you are asked, for that one task, \"Run it for real?\". " +
  "Turning it off only skips the dry run: every approval, the stop switch and the privacy " +
  "checks still apply.";

export const AGENT_ENABLED_NOTE =
  "Off by default. The agent also needs the activity log to be writable and the stop shortcut " +
  "to be registered. It asks before it acts and can be stopped at any time.";

export const DIAGNOSTIC_LOGS_NOTE =
  "Writes a small log of what the app did (never what you said, saw or typed) to a local folder, at most 10 MB, never sent anywhere. Takes effect the next time the app starts. Off by default.";
export const AGENT_MODE_NOTE =
  "Off by default. When on, what you say with push-to-talk goes to the agent as a task " +
  "instead of to the chat. Needs the agent to be enabled.";

/** Fixed core tokens for why the agent is not ready -> user text. */
export function agentReasonText(reason: string | null): string {
  switch (reason) {
    case null:
      return "Agent: ready";
    case "setting_off":
      return "Agent: off (enable it above)";
    case "not_ready":
      return "Agent: off until the activity log works and the stop shortcut is registered";
    case "no_runtime":
      return "Agent: not available on this machine";
    default:
      return "Agent: not available";
  }
}

export function agentStatusText(s: AgentStatusDto | null): string {
  if (!s) return "Agent: checking...";
  if (s.running) return "Agent: running a task";
  return agentReasonText(s.reason);
}

/** Fixed core tokens when a task could not start -> user text. */
export function agentStartErrorText(e: unknown): string {
  switch (e) {
    case "agent_disabled":
      return "The agent is not enabled or not ready";
    case "busy":
      return "A task is already running";
    case "bad_request":
      return "Type what the agent should do";
    case "agent_unavailable":
      return "The agent is not available on this machine";
    default:
      return errorText(e);
  }
}

/** One line for how a task ended. Tokens only; read answers are shown separately. */
export function agentOutcomeText(t: AgentTaskDto): string {
  const steps = `${t.steps_done} step${t.steps_done === 1 ? "" : "s"} done`;
  switch (t.outcome) {
    case "completed":
      return t.live
        ? `Done (${steps}, ${t.executed} action${t.executed === 1 ? "" : "s"})`
        : `Done (${steps})`;
    case "denied":
      return t.dry_run && !t.live
        ? `Dry run only: you chose not to run it (${t.would_do} step${t.would_do === 1 ? "" : "s"} shown)`
        : "You said no";
    case "expired":
      return "The approval timed out";
    case "killed":
      return "Stopped";
    case "stopped":
      return `Stopped by a limit (${t.detail})`;
    case "paused":
      return "Paused: it needs you";
    case "refused":
      return `Not started (${t.detail})`;
    default:
      return `Could not finish (${t.outcome}: ${t.detail})`;
  }
}
