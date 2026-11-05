import { describe, expect, it } from "vitest";
import src from "./DataPanel.tsx?raw";

import {
  CONFIRM_TOKENS,
  KINDS,
  arm,
  confirmArgs,
  dataErrorText,
  disarm,
  idle,
  resultText,
  sizeText,
} from "./logic";

describe("two-step confirm", () => {
  it("starts disarmed: nothing can be sent", () => {
    for (const k of KINDS) expect(confirmArgs(idle, k)).toBeNull();
  });
  it("the first click only arms that kind", () => {
    const s = arm("audit_log");
    expect(confirmArgs(s, "audit_log")).toEqual({ kind: "audit_log", confirm: "delete-audit-log" });
    expect(confirmArgs(s, "models")).toBeNull();
  });
  it("arming another kind disarms the first", () => {
    const first = arm("settings");
    expect(confirmArgs(first, "settings")).not.toBeNull();
    const s = arm("logs");
    expect(confirmArgs(s, "settings")).toBeNull();
    expect(confirmArgs(s, "logs")?.confirm).toBe("delete-logs");
  });
  it("cancel disarms", () => {
    expect(confirmArgs(disarm(), "models")).toBeNull();
  });
  it("every kind has its own distinct token", () => {
    const t = KINDS.map((k) => CONFIRM_TOKENS[k]);
    expect(new Set(t).size).toBe(KINDS.length);
    expect(t.every((x) => /^delete-[a-z-]+$/.test(x))).toBe(true);
  });
});

describe("text", () => {
  it("maps fixed tokens and falls back safely", () => {
    expect(dataErrorText("busy")).toMatch(/running/);
    expect(dataErrorText("needs_confirm")).toMatch(/Nothing was deleted/);
    expect(dataErrorText({ weird: 1 })).toMatch(/file error/);
  });
  it("sizes", () => {
    expect(sizeText({ kind: "logs", files: 0, bytes: 0 })).toBe("nothing stored");
    expect(sizeText({ kind: "logs", files: 1, bytes: 1024 })).toMatch(/^1 file, /);
    expect(sizeText({ kind: "logs", files: 3, bytes: 1024 })).toMatch(/^3 files, /);
  });
  it("results", () => {
    expect(resultText({ kind: "logs", removed: 0, failed: 0, restart_needed: false })).toMatch(/nothing to delete/);
    const r = resultText({ kind: "settings", removed: 1, failed: 2, restart_needed: true });
    expect(r).toMatch(/1 removed/);
    expect(r).toMatch(/2 could not/);
    expect(r).toMatch(/Restart/);
  });
});

describe("no delete on mount", () => {
  it("the panel calls dataDelete only from the confirmed click handler", () => {
    expect(src.match(/\.dataDelete\(/g)?.length).toBe(1);
    // Inside `run`, which starts by requiring the armed state.
    const run = src.slice(src.indexOf("const run ="), src.indexOf("return ("));
    expect(run).toContain("confirmArgs(step, kind)");
    expect(run).toContain("if (!args) return;");
    expect(run.indexOf("if (!args) return;")).toBeLessThan(run.indexOf("api"));
    // The mount effect only refreshes the overview.
    expect(src).toContain("useEffect(refresh, [refresh]);");
    expect(src.match(/\.dataOverview\(/g)?.length).toBe(1);
  });
});
