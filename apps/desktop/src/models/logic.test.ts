import { describe, expect, it } from "vitest";
import type { ModelRow } from "../ipc/ModelRow";
import {
  busy,
  canDownload,
  canLoadNow,
  diskOk,
  diskText,
  downloadErrorText,
  downloadLabel,
  emptyView,
  formatBytes,
  licenceText,
  needsExtraConfirm,
  onDone,
  onFailed,
  onProgress,
  percent,
  statusText,
} from "./logic";

const row: ModelRow = {
  id: "whisper-base",
  task: "stt",
  size_text: "74M params",
  licence: "MIT",
  commercial_ok: true,
  status: "missing",
  has_source: true,
  checksum_known: true,
  disk_needed: 100,
  disk_free: 1000,
  swap_slot: "stt",
};

describe("canDownload", () => {
  it("is enabled only for a missing model with a source, free slot and enough disk", () => {
    expect(canDownload(row, false)).toBe(true);
    expect(canDownload(row, true)).toBe(false);
    expect(canDownload({ ...row, has_source: false, status: "no_source" }, false)).toBe(false);
    expect(canDownload({ ...row, status: "ready" }, false)).toBe(false);
    expect(canDownload({ ...row, status: "downloading" }, false)).toBe(false);
    expect(canDownload({ ...row, status: "external" }, false)).toBe(false);
    expect(canDownload({ ...row, disk_free: 99 }, false)).toBe(false);
  });
  it("does not block on unknown sizes (the core checks again)", () => {
    expect(diskOk({ ...row, disk_needed: null })).toBe(true);
    expect(diskOk({ ...row, disk_free: null })).toBe(true);
  });
});

describe("texts", () => {
  it("shows licence for information and a note when not commercial-ok", () => {
    expect(licenceText(row)).toBe("Licence: MIT — shown for information");
    expect(licenceText({ ...row, commercial_ok: false })).toContain("Not marked for commercial use");
  });
  it("describes disk need versus free", () => {
    expect(diskText(row)).toBe("needs 100 B, 1000 B free");
    expect(diskText({ ...row, disk_needed: null })).toBe("size unknown, 1000 B free");
    expect(diskText({ ...row, disk_needed: null, disk_free: null })).toBe("unknown");
  });
  it("names every status, including checksum unknown and unverified", () => {
    expect(statusText({ ...row, checksum_known: false })).toContain("checksum unknown");
    expect(statusText({ ...row, status: "unverified" })).toContain("unverified");
    expect(statusText({ ...row, status: "no_source" })).toBe("no download source yet");
    expect(statusText({ ...row, status: "ready" })).toBe("ready");
  });
  it("formats bytes", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(488_000_000)).toBe("465 MB");
  });
  it("maps fixed tokens to text and never echoes unknown structure", () => {
    expect(downloadErrorText("checksum_mismatch")).toContain("deleted");
    expect(downloadErrorText("disk_space")).toContain("disk space");
    expect(downloadErrorText(42)).toBe("Something went wrong");
  });
});

describe("extra confirm", () => {
  it("is required exactly when the checksum is unknown", () => {
    expect(needsExtraConfirm(row)).toBe(false);
    expect(needsExtraConfirm({ ...row, checksum_known: false })).toBe(true);
  });
});

describe("download view reducer", () => {
  it("tracks progress, clears it on done and remembers finished models", () => {
    let v = onProgress(emptyView, { id: "a", bytes: 5, total: 10 });
    expect(percent(v.progress.a)).toBe(50);
    v = onDone(v, { id: "a" });
    expect(v.progress.a).toBeUndefined();
    expect(v.finished).toEqual(["a"]);
    expect(onDone(v, { id: "a" }).finished).toEqual(["a"]);
  });
  it("keeps the failure token for Retry, but a cancel is silent", () => {
    let v = onProgress(emptyView, { id: "a", bytes: 5, total: null });
    expect(percent(v.progress.a)).toBeNull();
    v = onFailed(v, { id: "a", code: "network" });
    expect(v.errors.a).toBe("network");
    expect(v.progress.a).toBeUndefined();
    expect(downloadLabel(row, { ...emptyView, errors: { [row.id]: "network" } })).toBe("Retry");
    expect(downloadLabel(row, emptyView)).toBe("Download");
    v = onFailed(v, { id: "a", code: "cancelled" });
    expect(v.errors.a).toBeUndefined();
  });
  it("a new progress event clears an old error (the retry started)", () => {
    const v = onProgress({ ...emptyView, errors: { a: "network" } }, { id: "a", bytes: 1, total: 2 });
    expect(v.errors.a).toBeUndefined();
  });
});

describe("Load now", () => {
  it("is offered after a download of a model the active profile needs", () => {
    const done = onDone(emptyView, { id: row.id });
    expect(canLoadNow({ ...row, status: "ready" }, done)).toBe(true);
    expect(canLoadNow({ ...row, status: "unverified" }, done)).toBe(true);
    expect(canLoadNow({ ...row, status: "ready" }, emptyView)).toBe(false);
    expect(canLoadNow({ ...row, status: "ready", swap_slot: null }, done)).toBe(false);
    expect(canLoadNow({ ...row, status: "missing" }, done)).toBe(false);
  });
});

describe("busy", () => {
  it("is true while any row downloads", () => {
    expect(busy([row])).toBe(false);
    expect(busy([row, { ...row, status: "downloading" }])).toBe(true);
  });
});
