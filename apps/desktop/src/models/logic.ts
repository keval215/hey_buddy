// Pure logic of the models table (unit-tested). No network: the core downloads, this only shows.
import type { DownloadDone } from "../ipc/DownloadDone";
import type { DownloadFailed } from "../ipc/DownloadFailed";
import type { DownloadProgress } from "../ipc/DownloadProgress";
import type { ModelRow } from "../ipc/ModelRow";

export type Progress = { bytes: number; total: number | null };

export type DownloadView = {
  /** Running downloads (at most one). */
  progress: Record<string, Progress>;
  /** Last failure token per model id. */
  errors: Record<string, string>;
  /** Models downloaded in this session: they may offer "Load now". */
  finished: string[];
};

export const emptyView: DownloadView = { progress: {}, errors: {}, finished: [] };

export function omit<T>(rec: Record<string, T>, key: string): Record<string, T> {
  return Object.fromEntries(Object.entries(rec).filter(([k]) => k !== key));
}

export function onProgress(v: DownloadView, e: DownloadProgress): DownloadView {
  return {
    ...v,
    progress: { ...v.progress, [e.id]: { bytes: e.bytes, total: e.total } },
    errors: omit(v.errors, e.id),
  };
}

export function onDone(v: DownloadView, e: DownloadDone): DownloadView {
  return {
    progress: omit(v.progress, e.id),
    errors: omit(v.errors, e.id),
    finished: v.finished.includes(e.id) ? v.finished : [...v.finished, e.id],
  };
}

export function onFailed(v: DownloadView, e: DownloadFailed): DownloadView {
  return {
    ...v,
    progress: omit(v.progress, e.id),
    // A cancel is not an error to show.
    errors: e.code === "cancelled" ? omit(v.errors, e.id) : { ...v.errors, [e.id]: e.code },
  };
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

/** Whole-percent progress, or null when the total is unknown. */
export function percent(p: Progress | undefined): number | null {
  if (!p || p.total === null || p.total <= 0) return null;
  return Math.min(100, Math.floor((p.bytes / p.total) * 100));
}

export function busy(rows: ModelRow[]): boolean {
  return rows.some((r) => r.status === "downloading");
}

/** Enough free space for the download (unknown numbers do not block; the core checks too). */
export function diskOk(r: ModelRow): boolean {
  return r.disk_needed === null || r.disk_free === null || r.disk_free >= r.disk_needed;
}

export function diskText(r: ModelRow): string {
  if (r.disk_needed === null) {
    return r.disk_free === null ? "unknown" : `size unknown, ${formatBytes(r.disk_free)} free`;
  }
  const need = `needs ${formatBytes(r.disk_needed)}`;
  return r.disk_free === null ? need : `${need}, ${formatBytes(r.disk_free)} free`;
}

export function statusText(r: ModelRow): string {
  switch (r.status) {
    case "missing":
      return r.checksum_known ? "not downloaded" : "not downloaded, checksum unknown";
    case "downloading":
      return "downloading";
    case "ready":
      return "ready";
    case "unverified":
      return "unverified (downloaded without a known checksum)";
    case "no_source":
      return "no download source yet";
    case "external":
      return "managed by Ollama";
  }
}

/** "Licence: X — shown for information", plus a visible note when not commercial-OK. */
export function licenceText(r: ModelRow): string {
  const base = `Licence: ${r.licence} — shown for information`;
  return r.commercial_ok ? base : `${base}. Not marked for commercial use.`;
}

/** The Download button is only ever enabled for a model that can be fetched right now. */
export function canDownload(r: ModelRow, anyBusy: boolean): boolean {
  return r.status === "missing" && r.has_source && !anyBusy && diskOk(r);
}

export function downloadLabel(r: ModelRow, view: DownloadView): string {
  return view.errors[r.id] ? "Retry" : "Download";
}

/** Rows with a checksum of `TODO` need the second confirm before the call is made. */
export function needsExtraConfirm(r: ModelRow): boolean {
  return !r.checksum_known;
}

/** After a successful download of a model the current profile uses: offer to load it. */
export function canLoadNow(r: ModelRow, view: DownloadView): boolean {
  return (
    r.swap_slot !== null &&
    view.finished.includes(r.id) &&
    (r.status === "ready" || r.status === "unverified")
  );
}

/** Fixed core tokens -> user text. */
export function downloadErrorText(code: unknown): string {
  switch (code) {
    case "busy":
      return "Another download is running";
    case "no_source":
      return "There is no download source for this model yet";
    case "checksum_unknown":
      return "The checksum is unknown; confirm to download anyway";
    case "already_present":
      return "The model is already on disk";
    case "disk_space":
      return "Not enough free disk space";
    case "disk_full":
      return "The disk became full; the partial file was removed";
    case "checksum_mismatch":
      return "The file did not match its checksum and was deleted";
    case "network":
      return "The connection failed or was interrupted (Retry resumes)";
    case "http_status":
      return "The server refused the download";
    case "bad_redirect":
    case "invalid_url":
      return "The download address is not allowed (HTTPS only)";
    case "too_large":
      return "The server sent more data than expected; the download was stopped";
    case "invalid_path":
      return "The registry has an unsafe file path; nothing was written";
    case "io_error":
      return "The file could not be written";
    case "unknown_model":
      return "Unknown model";
    case "unavailable":
      return "Downloads are not available in this build";
    case "cancelled":
      return "Cancelled";
    default:
      return typeof code === "string" && code.length > 0 ? code : "Something went wrong";
  }
}
