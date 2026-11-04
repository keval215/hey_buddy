// Pure helpers shared by the preview panel and the settings "Last request" view (unit-tested).
import type { LocalityDto } from "../ipc/LocalityDto";
import type { PurposeDto } from "../ipc/PurposeDto";

/** What both `PreviewDto` and `LastRequestDto` carry. */
export interface RequestView {
  provider_id: string;
  model: string;
  locality: LocalityDto;
  purpose: PurposeDto;
  digest_hex: string;
  transcript: string;
  screen_text: string | null;
  image_url: string | null;
  body_url: string;
  secure_fields_hidden: number;
}

/** Custom schemes are served as `http://<scheme>.localhost/...` on Windows (WebView2). */
export function isWindows(): boolean {
  return typeof navigator !== "undefined" && /windows/i.test(navigator.userAgent);
}

/**
 * Turn the core's `preview://<key>[/body]` into the URL this webview can load. The core
 * parses both shapes. `cacheKey` (the digest) keeps `preview://last` from being cached.
 */
export function previewSrc(url: string, windows: boolean, cacheKey?: string): string {
  const m = /^preview:\/\/([A-Za-z0-9]+)(\/body)?$/.exec(url);
  if (!m) return "";
  const q = cacheKey && /^[0-9a-f]+$/.test(cacheKey) ? `?d=${cacheKey}` : "";
  const path = `${m[1]}${m[2] ?? ""}`;
  return windows ? `http://preview.localhost/${path}${q}` : `preview://${path}${q}`;
}

export function shortDigest(hex: string): string {
  return hex.slice(0, 12);
}

export function isCloud(r: Pick<RequestView, "locality">): boolean {
  return r.locality === "Cloud";
}

export function badgeText(r: Pick<RequestView, "locality">): string {
  return isCloud(r) ? "CLOUD" : "LOCAL";
}

/** What the request is for: "Agent planning" for the agent engine's calls, else null. */
export function purposeText(r: Pick<RequestView, "purpose">): string | null {
  return r.purpose === "AgentPlan" ? "Agent planning" : null;
}

/** Whole seconds left until `deadlineMs`, never negative. */
export function secondsLeft(deadlineMs: number, nowMs: number): number {
  return Math.max(0, Math.ceil((deadlineMs - nowMs) / 1000));
}

export const RAW_VIEW_LIMIT = 200_000;

/** The raw body for display; says so when the view (not the request) is shortened. */
export function clipRaw(text: string, limit = RAW_VIEW_LIMIT): { text: string; clipped: boolean } {
  return text.length <= limit
    ? { text, clipped: false }
    : { text: text.slice(0, limit), clipped: true };
}
