import { describe, expect, it } from "vitest";
import { GUARD_MS, approveLabel, guardLeft, inert, shortDigest, titleFor } from "./logic";

describe("inert", () => {
  it("removes bidi overrides, zero-width, tag and control characters", () => {
    expect(inert("pay\u{202E}dne\u{200B}d\u{2066}x\u{2069}\u{FEFF}!")).toBe("paydnedx!");
    expect(inert("a\u{E0041}\u{E007F}b\u{7}c\u{0}d\u{E000}e")).toBe("abcde");
    expect(inert("a\u{AD}b\u{FE0F}c")).toBe("abc");
  });
  it("turns newlines and tabs into single spaces", () => {
    expect(inert("a\r\n\n\tb\u{2028}c  \u{A0} d")).toBe("a b c d");
  });
  it("leaves markup characters alone (React renders them as text)", () => {
    expect(inert("<script>alert(1)</script>")).toBe("<script>alert(1)</script>");
  });
  it("handles null and empty", () => {
    expect(inert(null)).toBe("");
    expect(inert(undefined)).toBe("");
  });
});

describe("misc", () => {
  it("shortDigest keeps 8 lowercase hex digits only", () => {
    expect(shortDigest("ABCDEF0123456789")).toBe("abcdef01");
    expect(shortDigest("<z>ab")).toBe("ab");
  });
  it("guard countdown", () => {
    expect(guardLeft(null, 5)).toBeNull();
    expect(guardLeft(1000, 1000)).toBe(GUARD_MS);
    expect(guardLeft(1000, 1000 + GUARD_MS)).toBe(0);
    expect(guardLeft(1000, 99_999)).toBe(0);
  });
  it("approve label shows the countdown", () => {
    expect(approveLabel(null)).toMatch(/waiting/);
    expect(approveLabel(400)).toBe("Approve (0.4 s)");
    expect(approveLabel(0)).toBe("Approve");
  });
  it("submit has its own title", () => {
    expect(titleFor("Submit")).not.toBe(titleFor("Step"));
    expect(titleFor("Plan")).toMatch(/plan/);
  });
});
