// Pure settings-form logic (unit-tested).
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
