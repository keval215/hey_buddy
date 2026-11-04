// Pure maths for the overlay cursor marker. No DOM, no Tauri: unit-tested.

/** Display geometry in the capture crate's units (points on macOS, physical px on Windows). */
export interface DisplayGeom {
  id: number;
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Pt {
  x: number;
  y: number;
}

export const MOVE_MS = 450;
export const HOLD_MS = 3000;
export const FADE_MS = 400;

/** Read `?id=&x=&y=&w=&h=` (set by the Rust side). `null` when anything is missing/invalid. */
export function parseDisplayParams(search: string): DisplayGeom | null {
  const q = new URLSearchParams(search);
  const get = (k: string): number | null => {
    const v = q.get(k);
    if (v === null || v.trim() === "") return null;
    const n = Number(v);
    return Number.isInteger(n) ? n : null;
  };
  const id = get("id");
  const x = get("x");
  const y = get("y");
  const w = get("w");
  const h = get("h");
  if (id === null || x === null || y === null || w === null || h === null) return null;
  if (w <= 0 || h <= 0 || id < 0) return null;
  return { id, x, y, w, h };
}

/** Does a `turn:point` belong to this overlay? */
export function isForDisplay(ev: { display_id: number }, d: DisplayGeom): boolean {
  return ev.display_id === d.id;
}

/**
 * Global point -> CSS px inside the overlay. Unit-agnostic: scales by viewport/display size so
 * it works for logical points (macOS) and physical pixels (Windows) alike. Clamped to the
 * viewport.
 */
export function toViewport(p: Pt, d: DisplayGeom, viewport: { w: number; h: number }): Pt {
  const clamp = (v: number, max: number) => Math.min(Math.max(v, 0), max);
  return {
    x: clamp(((p.x - d.x) * viewport.w) / d.w, viewport.w),
    y: clamp(((p.y - d.y) * viewport.h) / d.h, viewport.h),
  };
}

export function easeOutCubic(t: number): number {
  const c = Math.min(Math.max(t, 0), 1);
  return 1 - Math.pow(1 - c, 3);
}

export function markerAt(from: Pt, to: Pt, elapsedMs: number, durationMs = MOVE_MS): Pt {
  const e = easeOutCubic(durationMs <= 0 ? 1 : elapsedMs / durationMs);
  return { x: from.x + (to.x - from.x) * e, y: from.y + (to.y - from.y) * e };
}

/** Fully visible for `holdMs` after the point arrives, then a linear fade over `fadeMs`. */
export function opacityAt(sinceShownMs: number, holdMs = HOLD_MS, fadeMs = FADE_MS): number {
  if (sinceShownMs <= holdMs) return 1;
  if (sinceShownMs >= holdMs + fadeMs) return 0;
  return 1 - (sinceShownMs - holdMs) / fadeMs;
}
