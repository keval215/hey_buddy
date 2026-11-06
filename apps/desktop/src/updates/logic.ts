// Pure logic of the Updates section (unit-tested). It never talks to the network: the Rust core
// does, and only after a click. Every message is a fixed sentence keyed by a fixed token.
import type { DownloadDone } from "../ipc/DownloadDone";
import type { DownloadFailed } from "../ipc/DownloadFailed";
import type { DownloadProgress } from "../ipc/DownloadProgress";
import type { UpdateStatusDto } from "../ipc/UpdateStatusDto";
import { formatBytes } from "../models/logic";

/** The id the core puts on update download events. */
export const UPDATE_ID = "update";

export const NOT_CONFIGURED_TEXT = "Updates are not configured in this build.";
export const CHECK_NOTE =
  "Check for updates contacts the release host only when you click it. Nothing is checked or downloaded automatically.";
export const MANIFEST_NOTE_TITLE = "Release notes (plain text from the signed manifest)";
export const DOWNGRADE_WARNING =
  "Your current version has been withdrawn by the maintainer. You can go back to an older version. Only do this if you understand why.";
export const OUT_OF_DATE_NOTE =
  "Update information is out of date. Check the release page for newer versions.";

/** Fixed token (an error from a command or an event) to a human sentence. */
const ERROR_TEXT: Record<string, string> = {
  updates_not_configured: NOT_CONFIGURED_TEXT,
  busy: "Another update step is running. Wait for it to finish.",
  not_checked: "Check for updates first.",
  nothing_to_install: "There is nothing to install.",
  nothing_staged: "No verified installer is downloaded. Download it first.",
  version_mismatch: "The downloaded installer is for a different version. Check again.",
  downgrade_not_allowed: "That downgrade is not allowed.",
  revoked: "That version has been withdrawn and cannot be installed.",
  network: "Could not reach the release host.",
  http_status: "The release host answered with an error.",
  bad_redirect: "The release host redirected somewhere that is not allowed. Nothing was downloaded.",
  invalid_url: "The update address is not allowed. Nothing was downloaded.",
  too_large: "The update is larger than allowed. Nothing was downloaded.",
  malformed: "The update information is damaged or in an unknown format.",
  unknown_field: "The update information is damaged or in an unknown format.",
  bad_signature: "The update signature is wrong. Nothing was trusted.",
  no_trusted_signature: "The update is not signed by a key this app trusts.",
  wrong_product: "The update information is for a different product.",
  wrong_channel: "The update information is for a different channel.",
  clock_skew: "Your clock looks wrong (the update information is from the future).",
  manifest_expired: OUT_OF_DATE_NOTE,
  validity_too_long: "The update information has an invalid validity period.",
  stale_manifest: "The update information is older than this app. Nothing to do.",
  replayed_manifest: "The update information is older than one already seen. Nothing was trusted.",
  bad_url: "The update information points somewhere that is not allowed.",
  no_artifact: "There is no installer for this computer.",
  bad_version: "That version is not valid.",
  checksum_mismatch: "The download did not match its checksum and was deleted.",
  hash_mismatch: "The downloaded file changed and will not be started. Download it again.",
  unsafe_path: "The update folder is not safe to use. Nothing was started.",
  installer_needs_admin:
    "The installer asked for administrator rights, which this app never grants. Download it manually from the release page.",
  launch_blocked:
    "Windows or your security software blocked the installer. Download it manually from the release page.",
  unsupported_platform: "Updates cannot be installed on this system.",
  disk_space: "Not enough free disk space.",
  disk_full: "The disk is full. Nothing was kept.",
  io_error: "A file error stopped the update.",
  cancelled: "Download cancelled.",
  forbidden: "This window cannot do that.",
};

/** A fixed sentence for any token; unknown tokens never echo raw text. */
export function updateErrorText(e: unknown): string {
  const token = typeof e === "string" ? e : "";
  return ERROR_TEXT[token] ?? "The update step failed.";
}

export type UpdateView = {
  progress: { bytes: number; total: number | null } | null;
  /** Last download failure token (a cancel is not an error). */
  error: string | null;
  /** A download finished in this session. */
  finished: boolean;
};

export const emptyUpdateView: UpdateView = { progress: null, error: null, finished: false };

export function onUpdateProgress(v: UpdateView, e: DownloadProgress): UpdateView {
  if (e.id !== UPDATE_ID) return v;
  return { ...v, progress: { bytes: e.bytes, total: e.total }, error: null, finished: false };
}

export function onUpdateDone(v: UpdateView, e: DownloadDone): UpdateView {
  if (e.id !== UPDATE_ID) return v;
  return { progress: null, error: null, finished: true };
}

export function onUpdateFailed(v: UpdateView, e: DownloadFailed): UpdateView {
  if (e.id !== UPDATE_ID) return v;
  return { progress: null, error: e.code === "cancelled" ? null : e.code, finished: false };
}

/** The line under the check button. */
export function resultText(s: UpdateStatusDto): string {
  switch (s.result) {
    case "updates_not_configured":
      return NOT_CONFIGURED_TEXT;
    case "not_checked":
      return `You are running version ${s.current_version}. Not checked yet.`;
    case "up_to_date":
      return `You are up to date (version ${s.current_version}).`;
    case "available":
      return `Version ${s.version ?? "?"} is available${s.size ? ` (${formatBytes(s.size)})` : ""}. You are running ${s.current_version}.`;
    case "downgrade_only":
      return `Your version ${s.current_version} has been withdrawn. Version ${s.version ?? "?"} is offered as a downgrade.`;
  }
}

/** Extra advisory lines from the verified manifest flags. */
export function advisories(s: UpdateStatusDto): string[] {
  const out: string[] = [];
  if (s.current_revoked && s.result !== "downgrade_only") {
    out.push("Your current version has been withdrawn by the maintainer. Please update.");
  }
  if (s.below_min_supported) {
    out.push("Your version is no longer supported. Please update.");
  }
  if (s.state_issue) {
    out.push("The saved update state was unreadable and was reset. This is safe.");
  }
  return out;
}

export function isOffer(s: UpdateStatusDto): boolean {
  return s.result === "available" || s.result === "downgrade_only";
}

export function isDowngrade(s: UpdateStatusDto): boolean {
  return s.result === "downgrade_only";
}

/** "Check for updates" needs a configured build and no running step. */
export function canCheck(s: UpdateStatusDto | null, busyNow: boolean): boolean {
  return s !== null && s.configured && !s.busy && !busyNow;
}

/** "Download" needs an offer, nothing running and no verified installer yet. */
export function canDownload(
  s: UpdateStatusDto | null,
  v: UpdateView,
  busyNow: boolean,
): boolean {
  return (
    s !== null &&
    s.configured &&
    isOffer(s) &&
    !s.busy &&
    !busyNow &&
    v.progress === null &&
    !s.downloaded
  );
}

/** "Install and restart" is a separate click that needs a downloaded, verified installer. */
export function canInstall(s: UpdateStatusDto | null, v: UpdateView, busyNow: boolean): boolean {
  return (
    s !== null &&
    s.configured &&
    isOffer(s) &&
    s.downloaded &&
    !s.busy &&
    !busyNow &&
    v.progress === null
  );
}

/**
 * What `update_install` is called with. A downgrade only goes out after the explicit confirm
 * (`confirmed`); an upgrade never sends the flag.
 */
export function installArgs(
  s: UpdateStatusDto,
  confirmed: boolean,
): { version: string; allowDowngrade: boolean } | null {
  if (!isOffer(s) || s.version === null) return null;
  if (isDowngrade(s)) {
    return confirmed ? { version: s.version, allowDowngrade: true } : null;
  }
  return { version: s.version, allowDowngrade: false };
}

export function percent(v: UpdateView): number | null {
  const p = v.progress;
  if (!p || p.total === null || p.total <= 0) return null;
  return Math.min(100, Math.floor((p.bytes / p.total) * 100));
}
