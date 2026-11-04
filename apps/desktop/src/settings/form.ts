// Pure settings-form logic (unit-tested).
import type { EngineSlotName } from "../ipc/EngineSlotName";
import type { EngineSwapEvent } from "../ipc/EngineSwapEvent";
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

/** The profile decides the engines: a profile change is what needs a swap. */
export function needsEngineSwap(patch: SettingsPatch): boolean {
  return patch.profile !== undefined;
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
