// Pure settings-form logic (unit-tested).
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
  const dir = draft.models_dir.trim();
  if (dir !== current.models_dir) patch.models_dir = dir;
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
