import { describe, expect, it } from "vitest";
import { badgeText, clipRaw, isCloud, previewSrc, purposeText, secondsLeft, shortDigest } from "./logic";

describe("previewSrc", () => {
  it("keeps preview:// on macOS/Linux", () => {
    expect(previewSrc("preview://12", false)).toBe("preview://12");
    expect(previewSrc("preview://12/body", false)).toBe("preview://12/body");
    expect(previewSrc("preview://last", false, "ab12")).toBe("preview://last?d=ab12");
  });
  it("maps to http://preview.localhost on Windows", () => {
    expect(previewSrc("preview://12", true)).toBe("http://preview.localhost/12");
    expect(previewSrc("preview://12/body", true)).toBe("http://preview.localhost/12/body");
    expect(previewSrc("preview://last", true, "ab12")).toBe("http://preview.localhost/last?d=ab12");
  });
  it("rejects anything else and ignores odd cache keys", () => {
    expect(previewSrc("https://evil.example/x", false)).toBe("");
    expect(previewSrc("preview://12/../x", false)).toBe("");
    expect(previewSrc("javascript:alert(1)", true)).toBe("");
    expect(previewSrc("preview://12", false, "x&y")).toBe("preview://12");
  });
});

describe("misc", () => {
  it("badge", () => {
    expect(badgeText({ locality: "Cloud" })).toBe("CLOUD");
    expect(badgeText({ locality: "Local" })).toBe("LOCAL");
    expect(isCloud({ locality: "Cloud" })).toBe(true);
  });
  it("purpose text", () => {
    expect(purposeText({ purpose: "AgentPlan" })).toBe("Agent planning");
    expect(purposeText({ purpose: "Turn" })).toBeNull();
  });
  it("shortDigest", () => expect(shortDigest("0123456789abcdef0123")).toBe("0123456789ab"));
  it("secondsLeft rounds up and never goes negative", () => {
    expect(secondsLeft(10_000, 0)).toBe(10);
    expect(secondsLeft(10_000, 9_001)).toBe(1);
    expect(secondsLeft(10_000, 11_000)).toBe(0);
  });
  it("clipRaw", () => {
    expect(clipRaw("abc", 5)).toEqual({ text: "abc", clipped: false });
    expect(clipRaw("abcdef", 5)).toEqual({ text: "abcde", clipped: true });
  });
});
