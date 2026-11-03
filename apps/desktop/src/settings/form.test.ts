import { describe, expect, it } from "vitest";
import type { Settings } from "../ipc/Settings";
import {
  buildPatch,
  errorText,
  isEmptyPatch,
  keyErrorText,
  keyStatusText,
  shownVision,
  visionLocked,
} from "./form";

const base: Settings = {
  local_only: true,
  vision_mode: "Auto",
  profile: "default",
  hotkey: "Ctrl+Alt+Space",
  has_api_key: false,
  anthropic_model: "",
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
  it("sends a trimmed model id only when changed", () => {
    expect(buildPatch(base, { ...base, anthropic_model: " some-model " })).toEqual({
      anthropic_model: "some-model",
    });
    expect(isEmptyPatch(buildPatch(base, { ...base, anthropic_model: "  " }))).toBe(true);
  });
  it("never puts has_api_key in a patch", () => {
    expect(buildPatch(base, { ...base, has_api_key: true })).toEqual({});
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

describe("key texts", () => {
  it("maps the fixed tokens", () => {
    expect(keyErrorText("credential_store_unsupported")).toMatch(/not available/);
    expect(keyErrorText("invalid_key")).toMatch(/valid/);
    expect(keyErrorText(42)).toBe("Something went wrong");
  });
  it("shows only masked status", () => {
    expect(keyStatusText(true)).toMatch(/hidden/);
    expect(keyStatusText(false)).toMatch(/not set/);
  });
});
