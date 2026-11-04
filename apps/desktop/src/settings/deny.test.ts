import { describe, expect, it } from "vitest";
import type { DenyListDto } from "../ipc/DenyListDto";
import {
  askRemove,
  checkAppId,
  denyErrorText,
  isPendingRemove,
  loadIssueText,
  removedText,
  splitEntries,
} from "./deny";

const view: DenyListDto = {
  entries: [
    { id: "cmd.exe", builtin: true },
    { id: "mine.exe", builtin: false },
  ],
  removed_defaults: [],
  banking_notice: "n",
  load_issue: null,
};

describe("checkAppId", () => {
  it("accepts and lowercases valid ids", () => {
    expect(checkAppId("  Com.Bank.App ")).toEqual({ ok: true, id: "com.bank.app" });
    expect(checkAppId("a")).toEqual({ ok: true, id: "a" });
    expect(checkAppId("a".repeat(128)).ok).toBe(true);
  });
  it("rejects bad ids with a reason", () => {
    for (const bad of ["", "   ", "a b", "a/b", "a\\b", "..\\x", "a\nb", "a\u202eb", "é", "a;b", "a".repeat(129)]) {
      const r = checkAppId(bad);
      expect(r.ok, JSON.stringify(bad)).toBe(false);
      if (!r.ok) expect(r.reason.length).toBeGreaterThan(5);
    }
  });
});

describe("texts", () => {
  it("maps the core tokens", () => {
    expect(denyErrorText("would_be_empty")).toMatch(/never be empty/);
    expect(denyErrorText("needs_confirm")).toMatch(/confirm/);
    expect(denyErrorText("busy")).toMatch(/Stop it first/);
    expect(denyErrorText("zzz")).toBe("zzz");
    expect(denyErrorText(1)).toBe("Something went wrong");
  });
  it("describes a broken file and removed built-ins", () => {
    expect(loadIssueText(null)).toBeNull();
    expect(loadIssueText("invalid")).toMatch(/built-in list is in use/);
    expect(loadIssueText("would_be_empty")).toMatch(/empty/);
    expect(removedText(view)).toBeNull();
    expect(removedText({ ...view, removed_defaults: ["cmd.exe"] })).toBe(
      "You removed 1 built-in app from the list.",
    );
    expect(removedText({ ...view, removed_defaults: ["a", "b"] })).toMatch(/2 built-in apps/);
  });
});

describe("list and confirm step", () => {
  it("splits built-in and user entries", () => {
    const s = splitEntries(view);
    expect(s.builtin.map((e) => e.id)).toEqual(["cmd.exe"]);
    expect(s.user.map((e) => e.id)).toEqual(["mine.exe"]);
  });
  it("removes a user entry at once but asks first for a built-in one", () => {
    expect(askRemove(view.entries[1]!)).toEqual({ pending: null, immediate: true });
    const a = askRemove(view.entries[0]!);
    expect(a.immediate).toBe(false);
    expect(isPendingRemove(a.pending, "cmd.exe")).toBe(true);
    expect(isPendingRemove(a.pending, "other.exe")).toBe(false);
    expect(isPendingRemove(null, "cmd.exe")).toBe(false);
  });
});
