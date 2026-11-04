// Pure logic of the "Agent activity log" section (unit-tested). The core owns every rule; this
// formats rows, keeps the paging stack, gives quick export-path feedback and turns the core's
// fixed error tokens into plain sentences. Entries are content-free (ids, digests, fixed tokens
// and a sanitised label), and everything is rendered as text.
import type { AuditEntryDto } from "../ipc/AuditEntryDto";
import type { AuditPageDto } from "../ipc/AuditPageDto";

/** `2024-05-01 12:30:45 UTC` (UTC so the table reads the same everywhere). */
export function formatTime(tsMs: number): string {
  const d = new Date(tsMs);
  if (!Number.isFinite(d.getTime())) return "unknown time";
  return `${d.toISOString().slice(0, 19).replace("T", " ")} UTC`;
}

/** `snake_case` token -> words. */
export function humanise(token: string | null): string {
  if (!token) return "";
  return token.replace(/_/g, " ");
}

export interface AuditRow {
  key: string;
  time: string;
  task: string;
  step: string;
  action: string;
  target: string;
  app: string;
  level: string;
  decision: string;
  result: string;
}

/** One table row. Steps are shown counting from 1. */
export function toRow(e: AuditEntryDto): AuditRow {
  const target = [humanise(e.target_role), e.target_label ? `"${e.target_label}"` : ""]
    .filter((s) => s !== "")
    .join(" ");
  const typed = e.text_len != null ? ` (${e.text_len} characters)` : "";
  const extra = e.segments_removed != null ? ` (${e.segments_removed} files removed)` : "";
  return {
    key: `${e.session_id}-${e.seq}`,
    time: formatTime(e.ts_ms),
    task: e.task != null ? String(e.task) : "",
    step: e.step != null ? String(e.step + 1) : "",
    action: `${humanise(e.op) || humanise(e.event)}${typed}${extra}`,
    target,
    app: e.app_id ?? "",
    level: humanise(e.tier),
    decision: humanise(e.decision),
    result: humanise(e.result_code),
  };
}

/** Pages are newest first; the stack holds the cursor of every page shown so far. */
export type Pager = { cursors: number[] };

export const firstPage: Pager = { cursors: [0] };

export function currentCursor(p: Pager): number {
  return p.cursors[p.cursors.length - 1] ?? 0;
}

/** Go to the older page (only when the core gave a next cursor). */
export function older(p: Pager, next: number | null): Pager {
  return next == null ? p : { cursors: [...p.cursors, next] };
}

/** Go back to the newer page (never below the first). */
export function newer(p: Pager): Pager {
  return p.cursors.length <= 1 ? p : { cursors: p.cursors.slice(0, -1) };
}

export function canNewer(p: Pager): boolean {
  return p.cursors.length > 1;
}

export function pageLabel(p: Pager, page: AuditPageDto): string {
  const n = page.entries.length;
  if (n === 0) return "No entries";
  const from = currentCursor(p) + 1;
  return `Entries ${from} to ${from + n - 1} (newest first)`;
}

export function corruptNotice(n: number): string | null {
  if (n <= 0) return null;
  return n === 1
    ? "1 line in the log could not be read and is not shown."
    : `${n} lines in the log could not be read and are not shown.`;
}

export function limitNotice(page: AuditPageDto): string | null {
  return page.over_limit
    ? `The log is spread over ${page.segments} files. Nothing is ever deleted automatically: export it, then clear it, when you like.`
    : null;
}

/** Control, bidi, zero-width and line-separator characters. */
function isHostileChar(c: string): boolean {
  const n = c.codePointAt(0) ?? 0;
  return (
    n < 0x20 ||
    (n >= 0x7f && n <= 0x9f) ||
    (n >= 0x200b && n <= 0x200f) ||
    (n >= 0x2028 && n <= 0x202e) ||
    (n >= 0x2060 && n <= 0x2069) ||
    n === 0xfeff
  );
}

/** Quick check of the typed export path (the core checks again). */
export function checkExportPath(text: string): { ok: true; path: string } | { ok: false; reason: string } {
  const t = text.trim();
  if (t === "") return { ok: false, reason: "Enter where the file should go." };
  if (t.length > 1024) return { ok: false, reason: "That path is too long." };
  if ([...t].some(isHostileChar)) {
    return { ok: false, reason: "The path contains characters that are not allowed." };
  }
  if (t.startsWith("\\\\") || t.startsWith("//")) {
    return { ok: false, reason: "Network paths are not allowed." };
  }
  if (!(t.startsWith("/") || /^[A-Za-z]:[\\/]/.test(t))) {
    return { ok: false, reason: "Use a full path, for example C:\\Users\\me\\Documents\\buddy-log.jsonl." };
  }
  if (t.split(/[\\/]/).includes("..")) {
    return { ok: false, reason: "The path must not contain \"..\"." };
  }
  return { ok: true, path: t };
}

/** Fixed core tokens -> user text. */
export function auditErrorText(e: unknown): string {
  switch (e) {
    case "unavailable":
      return "The activity log is not available.";
    case "needs_confirm":
      return "Please confirm first.";
    case "invalid_path":
      return "That location is not allowed. Pick an existing folder such as Documents.";
    case "exists":
      return "A file with that name already exists. Nothing was overwritten.";
    case "io_error":
      return "That did not work, so nothing was changed.";
    default:
      return "Something went wrong";
  }
}

/** The two-step clear: the first click only asks; nothing is deleted until the second. */
export type ClearStep = "idle" | "confirm";

export function clickClear(step: ClearStep): { step: ClearStep; send: boolean } {
  return step === "idle" ? { step: "confirm", send: false } : { step: "idle", send: true };
}

export function cancelClear(): ClearStep {
  return "idle";
}

export const NOT_READY_TEXT =
  "The activity log is not working, so the agent stays off until it can be written.";
