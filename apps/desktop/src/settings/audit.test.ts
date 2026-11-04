import { describe, expect, it } from "vitest";
import type { AuditEntryDto } from "../ipc/AuditEntryDto";
import type { AuditPageDto } from "../ipc/AuditPageDto";
import {
  auditErrorText,
  canNewer,
  cancelClear,
  checkExportPath,
  clickClear,
  corruptNotice,
  currentCursor,
  firstPage,
  formatTime,
  limitNotice,
  newer,
  older,
  pageLabel,
  toRow,
} from "./audit";

const entry: AuditEntryDto = {
  ts_ms: 1714566645000,
  session_id: "s1",
  task: 7,
  seq: 3,
  event: "intent",
  plan_digest8: "abcdef01",
  action_digest8: "01234567",
  step: 1,
  op: "type",
  target_role: "text_field",
  target_label: "Name",
  app_id: "notepad.exe",
  tier: "reversible",
  decision: "allow",
  result_code: "ok",
  text_len: 11,
  segments_removed: null,
};

const page = (n: number, over = false): AuditPageDto => ({
  entries: Array.from({ length: n }, () => entry),
  corrupt_lines: 0,
  next_cursor: null,
  segments: 1,
  over_limit: over,
  ready: true,
});

describe("rows", () => {
  it("formats a row in plain words", () => {
    expect(toRow(entry)).toEqual({
      key: "s1-3",
      time: "2024-05-01 12:30:45 UTC",
      task: "7",
      step: "2",
      action: "type (11 characters)",
      target: 'text field "Name"',
      app: "notepad.exe",
      level: "reversible",
      decision: "allow",
      result: "ok",
    });
  });
  it("copes with empty fields and the cleared line", () => {
    const r = toRow({
      ...entry,
      event: "cleared",
      op: null,
      task: null,
      step: null,
      target_role: null,
      target_label: null,
      app_id: null,
      tier: null,
      decision: null,
      result_code: null,
      text_len: null,
      segments_removed: 3,
    });
    expect(r.action).toBe("cleared (3 files removed)");
    expect(r.target + r.task + r.step + r.app).toBe("");
  });
  it("keeps hostile text as plain strings (rendering is text only)", () => {
    const r = toRow({ ...entry, target_label: "<img src=x onerror=alert(1)>" });
    expect(r.target).toContain("<img");
  });
  it("formats time safely", () => {
    expect(formatTime(0)).toBe("1970-01-01 00:00:00 UTC");
    expect(formatTime(Number.NaN)).toBe("unknown time");
  });
});

describe("paging", () => {
  it("walks older and back, never below the first page", () => {
    let p = firstPage;
    expect(currentCursor(p)).toBe(0);
    expect(canNewer(p)).toBe(false);
    expect(newer(p)).toBe(p);
    expect(older(p, null)).toBe(p);
    p = older(p, 50);
    p = older(p, 100);
    expect(currentCursor(p)).toBe(100);
    expect(canNewer(p)).toBe(true);
    p = newer(p);
    expect(currentCursor(p)).toBe(50);
    p = newer(p);
    expect(currentCursor(p)).toBe(0);
    expect(newer(p)).toBe(p);
  });
  it("labels the page", () => {
    expect(pageLabel(firstPage, page(0))).toBe("No entries");
    expect(pageLabel(older(firstPage, 50), page(10))).toBe("Entries 51 to 60 (newest first)");
  });
});

describe("notices", () => {
  it("corrupt lines", () => {
    expect(corruptNotice(0)).toBeNull();
    expect(corruptNotice(1)).toBe("1 line in the log could not be read and is not shown.");
    expect(corruptNotice(4)).toBe("4 lines in the log could not be read and are not shown.");
  });
  it("over the soft limit", () => {
    expect(limitNotice(page(1))).toBeNull();
    expect(limitNotice(page(1, true))).toContain("Nothing is ever deleted automatically");
  });
});

describe("export path", () => {
  it("accepts absolute paths", () => {
    expect(checkExportPath("  /home/me/Documents/log.jsonl ")).toEqual({
      ok: true,
      path: "/home/me/Documents/log.jsonl",
    });
    expect(checkExportPath("C:\\Users\\me\\log.jsonl").ok).toBe(true);
    expect(checkExportPath("D:/x/log.jsonl").ok).toBe(true);
  });
  it("rejects bad paths with a reason", () => {
    for (const bad of [
      "",
      "   ",
      "log.jsonl",
      "relative/log.jsonl",
      "C:log.jsonl",
      "/a/../b.jsonl",
      "C:\\a\\..\\b.jsonl",
      "\\\\server\\share\\x",
      "//server/share/x",
      "/a/b\nc",
      "/a/b\u202ec",
      "/" + "a".repeat(1100),
    ]) {
      const r = checkExportPath(bad);
      expect(r.ok, bad).toBe(false);
    }
  });
});

describe("clear confirm", () => {
  it("needs two clicks and cancel goes back", () => {
    let r = clickClear("idle");
    expect(r).toEqual({ step: "confirm", send: false });
    expect(cancelClear()).toBe("idle");
    r = clickClear(r.step);
    expect(r).toEqual({ step: "idle", send: true });
  });
});

describe("error text", () => {
  it("maps tokens and never echoes unknown input", () => {
    expect(auditErrorText("exists")).toContain("already exists");
    expect(auditErrorText("needs_confirm")).toBe("Please confirm first.");
    expect(auditErrorText("PRIVATE_MARKER")).toBe("Something went wrong");
    expect(auditErrorText(undefined)).toBe("Something went wrong");
  });
});
