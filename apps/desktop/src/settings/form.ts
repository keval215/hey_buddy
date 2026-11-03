// Pure settings-form logic (unit-tested).
import type { Phase } from "../ipc/Phase";
import type { Settings } from "../ipc/Settings";
import type { SettingsPatch } from "../ipc/SettingsPatch";
import type { VisionMode } from "../ipc/VisionMode";

export const PROFILES = ["default", "low-memory"] as const;
export const VISION_MODES: VisionMode[] = ["Off", "Auto", "Always"];

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
  if (!visionLocked(draft.profile) && draft.vision_mode !== current.vision_mode) {
    patch.vision_mode = draft.vision_mode;
  }
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
