// Pure deny-list editor logic (unit-tested). The core validates and enforces every rule; this
// gives quick feedback and turns its fixed tokens into plain sentences.
import type { DenyEntryDto } from "../ipc/DenyEntryDto";
import type { DenyListDto } from "../ipc/DenyListDto";

export const ID_PATTERN = /^[A-Za-z0-9._-]{1,128}$/;

export type IdCheck = { ok: true; id: string } | { ok: false; reason: string };

/** Quick check of an app id as typed (the core checks again). */
export function checkAppId(text: string): IdCheck {
  const t = text.trim();
  if (t === "") return { ok: false, reason: "Enter an app id." };
  if (t.length > 128) return { ok: false, reason: "That id is too long (128 characters at most)." };
  if (!ID_PATTERN.test(t)) {
    return {
      ok: false,
      reason: "Use only letters, digits, dot, dash and underscore (no spaces or slashes).",
    };
  }
  return { ok: true, id: t.toLowerCase() };
}

/** Fixed core tokens -> user text. */
export function denyErrorText(e: unknown): string {
  switch (e) {
    case "invalid_id":
      return "That is not a valid app id.";
    case "already_listed":
      return "That app is already on the list.";
    case "not_found":
      return "That app is not on the list.";
    case "needs_confirm":
      return "Please confirm first.";
    case "would_be_empty":
      return "The list can never be empty, so the last entry has to stay.";
    case "io_error":
      return "The change could not be saved, so nothing was changed.";
    case "unavailable":
      return "The deny list is not available yet.";
    default:
      return typeof e === "string" && e.length > 0 ? e : "Something went wrong";
  }
}

/** Load-problem token -> text. */
export function loadIssueText(token: string | null): string | null {
  switch (token) {
    case null:
      return null;
    case "unreadable":
      return "The deny-list file could not be read, so the built-in list is in use.";
    case "would_be_empty":
      return "The deny-list file would have left the list empty, so the built-in list is in use.";
    default:
      return "The deny-list file is not valid, so the built-in list is in use.";
  }
}

export function splitEntries(v: DenyListDto): { builtin: DenyEntryDto[]; user: DenyEntryDto[] } {
  return {
    builtin: v.entries.filter((e) => e.builtin),
    user: v.entries.filter((e) => !e.builtin),
  };
}

export function removedText(v: DenyListDto): string | null {
  const n = v.removed_defaults.length;
  if (n === 0) return null;
  return n === 1
    ? "You removed 1 built-in app from the list."
    : `You removed ${n} built-in apps from the list.`;
}

/** The two-step confirm: nothing is removed from the built-in list on the first click. */
export type Pending = { kind: "remove"; id: string } | { kind: "reset" } | null;

export function askRemove(entry: DenyEntryDto): { pending: Pending; immediate: boolean } {
  return entry.builtin
    ? { pending: { kind: "remove", id: entry.id }, immediate: false }
    : { pending: null, immediate: true };
}

export function isPendingRemove(p: Pending, id: string): boolean {
  return p?.kind === "remove" && p.id === id;
}
