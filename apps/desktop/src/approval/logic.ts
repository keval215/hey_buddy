// Pure helpers of the approval window (unit-tested). Everything shown goes through `inert`.
import type { ApprovalKindDto } from "../ipc/ApprovalKindDto";
import type { ApprovalTierDto } from "../ipc/ApprovalTierDto";

/** Approve stays disabled this long after the request was displayed (ADR 0003 section 7). */
export const GUARD_MS = 500;

// Control (Cc), format incl. bidi and tag characters (Cf), private use (Co), line/paragraph
// separators and variation selectors. Core already strips these; this is the second line.
const HOSTILE = /[\p{Cc}\p{Cf}\p{Co}\p{Variation_Selector}\u{2028}\u{2029}]/gu;

/** Text safe to render: whitespace controls become a space, hostile characters are removed. */
export function inert(s: string | null | undefined): string {
  if (!s) return "";
  return s.replace(/[\n\r\t\v\f\u{85}\u{2028}\u{2029}\p{Zs}]+/gu, " ").replace(HOSTILE, "").replace(/ {2,}/g, " ").trim();
}

export function shortDigest(hex: string): string {
  return inert(hex).replace(/[^0-9a-f]/gi, "").slice(0, 8).toLowerCase();
}

export function tierLabel(t: ApprovalTierDto): string {
  return t;
}

export function titleFor(kind: ApprovalKindDto): string {
  switch (kind) {
    case "Plan":
      return "Approve this plan?";
    case "Step":
      return "Approve this action?";
    case "Submit":
      return "Approve SUBMIT (sends data out)?";
  }
}

/** Milliseconds until Approve unlocks (0 = unlocked); `null` shownAt = not yet acknowledged. */
export function guardLeft(shownAt: number | null, now: number): number | null {
  if (shownAt === null) return null;
  return Math.max(0, shownAt + GUARD_MS - now);
}

/** Label for the Approve button including its countdown state. */
export function approveLabel(left: number | null): string {
  if (left === null) return "Approve (waiting for the window)";
  if (left > 0) return `Approve (${(left / 1000).toFixed(1)} s)`;
  return "Approve";
}
