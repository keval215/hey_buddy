import { describe, expect, it } from "vitest";
import type { Settings } from "../ipc/Settings";
import { buildPatch, errorText, isEmptyPatch, shownVision, visionLocked } from "./form";

const base: Settings = {
  local_only: true,
  vision_mode: "Auto",
  profile: "default",
  hotkey: "Ctrl+Alt+Space",
  has_api_key: false,
};

describe("buildPatch", () => {
  it("is empty without changes", () => {
    expect(isEmptyPatch(buildPatch(base, { ...base }))).toBe(true);
  });
  it("contains only changed fields", () => {
    expect(buildPatch(base, { ...base, local_only: false })).toEqual({ local_only: false });
    expect(buildPatch(base, { ...base, vision_mode: "Always" })).toEqual({
      vision_mode: "Always",
    });
  });
  it("trims the hotkey", () => {
    expect(buildPatch(base, { ...base, hotkey: "  Alt+K " })).toEqual({ hotkey: "Alt+K" });
  });
  it("never sends a vision change for low-memory", () => {
    const p = buildPatch(base, { ...base, profile: "low-memory", vision_mode: "Always" });
    expect(p).toEqual({ profile: "low-memory" });
  });
});

describe("vision lock", () => {
  it("locks and shows Off on low-memory only", () => {
    expect(visionLocked("low-memory")).toBe(true);
    expect(visionLocked("default")).toBe(false);
    expect(shownVision({ ...base, profile: "low-memory", vision_mode: "Always" })).toBe("Off");
    expect(shownVision({ ...base, vision_mode: "Always" })).toBe("Always");
  });
});

describe("errorText", () => {
  it("passes strings and falls back otherwise", () => {
    expect(errorText("invalid hotkey")).toBe("invalid hotkey");
    expect(errorText(new Error("x"))).toBe("Something went wrong");
    expect(errorText("")).toBe("Something went wrong");
  });
});
