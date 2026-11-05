// Pure logic of the "Your data" section (unit-tested). Nothing here deletes anything: the Rust
// core does, and only for a command that carries the exact confirmation token of the kind.
import type { DataDeleteDto } from "../ipc/DataDeleteDto";
import type { DataKind } from "../ipc/DataKind";
import type { DataKindDto } from "../ipc/DataKindDto";
import { formatBytes } from "../models/logic";

/** Order shown in the table and the buttons. */
export const KINDS: readonly DataKind[] = [
  "settings",
  "deny_list",
  "audit_log",
  "models",
  "updates",
  "logs",
];

/** Must match `DataKind::confirm_token` in the core; the core refuses any other word. */
export const CONFIRM_TOKENS: Record<DataKind, string> = {
  settings: "delete-settings",
  deny_list: "delete-deny-list",
  audit_log: "delete-audit-log",
  models: "delete-models",
  updates: "delete-updates",
  logs: "delete-logs",
};

export const KIND_LABEL: Record<DataKind, string> = {
  settings: "Settings",
  deny_list: "App deny list changes",
  audit_log: "Agent activity log",
  models: "Downloaded models",
  updates: "Downloaded update files",
  logs: "Diagnostic logs",
};

/** What each delete removes, and the one thing to know before clicking. */
export const KIND_WARNING: Record<DataKind, string> = {
  settings:
    "Resets every setting and the first-run wizard to the defaults at the next start. Your API key in the system keychain is not touched.",
  deny_list: "Removes your additions to and removals from the app deny list. The built-in list applies again at the next start.",
  audit_log:
    "The activity log is append-only on purpose. This erases all of it for good; a new log starts with one line saying it was erased.",
  models: "Removes the downloaded model files. You can download them again. A models folder you chose yourself is never touched.",
  updates: "Removes downloaded installers. The record that prevents old updates from being replayed is kept.",
  logs: "Removes the diagnostic log files.",
};

export const BUSY_TEXT = "Something is running (a task, a request, a download or an update). Try again when it has finished.";

/** Fixed core token to sentence. */
export function dataErrorText(code: unknown): string {
  switch (code) {
    case "busy":
      return BUSY_TEXT;
    case "needs_confirm":
      return "The confirmation did not match. Nothing was deleted.";
    case "unavailable":
      return "This data is not available right now. Nothing was deleted.";
    case "forbidden":
      return "This window cannot do that.";
    default:
      return "A file error stopped the deletion. Some files may remain.";
  }
}

/** Two-step confirm per kind: nothing is deleted by the first click. */
export type ConfirmStep = { kind: DataKind | null };

export const idle: ConfirmStep = { kind: null };

/** First click on a delete button: only arms that one kind (arming another disarms the first). */
export function arm(kind: DataKind): ConfirmStep {
  return { kind };
}

export function disarm(): ConfirmStep {
  return idle;
}

/** The second click: the kind and token to send, or `null` when that kind is not armed. */
export function confirmArgs(
  s: ConfirmStep,
  kind: DataKind,
): { kind: DataKind; confirm: string } | null {
  return s.kind === kind ? { kind, confirm: CONFIRM_TOKENS[kind] } : null;
}

export function sizeText(row: DataKindDto): string {
  if (row.files === 0) return "nothing stored";
  const n = row.files === 1 ? "1 file" : `${row.files} files`;
  return `${n}, ${formatBytes(row.bytes)}`;
}

/** Result sentence from fixed numbers. */
export function resultText(r: DataDeleteDto): string {
  const label = KIND_LABEL[r.kind];
  const base = r.removed === 0 && r.failed === 0 ? `${label}: nothing to delete.` : `${label}: ${r.removed} removed.`;
  const failed = r.failed > 0 ? ` ${r.failed} could not be removed (or was a link and was left alone).` : "";
  const restart = r.restart_needed ? " Restart the app to start from the defaults." : "";
  return base + failed + restart;
}

/** A delete button is enabled only while no delete is running. */
export function canDelete(working: boolean): boolean {
  return !working;
}
