import { describe, expect, it } from "vitest";
import type { Settings } from "../ipc/Settings";
import {
  applySwap,
  buildPatch,
  errorText,
  isEmptyPatch,
  keyErrorText,
  keyStatusText,
  needsEngineSwap,
  shownVision,
  swapErrorText,
  swapText,
  visionLocked,
} from "./form";

const base: Settings = {
  local_only: true,
  vision_mode: "Auto",
  profile: "default",
  hotkey: "Ctrl+Alt+Space",
  has_api_key: false,
  provider: "Ollama",
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
  it("sends the provider only when changed", () => {
    expect(buildPatch(base, { ...base, provider: "Anthropic" })).toEqual({
      provider: "Anthropic",
    });
    expect(isEmptyPatch(buildPatch(base, { ...base, provider: "Ollama" }))).toBe(true);
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

describe("engine swap status", () => {
  const ev = { slot: "stt", engine_id: "whisper-base", error: null, path: null } as const;
  it("only a profile change needs a swap", () => {
    expect(needsEngineSwap({ profile: "low-memory" })).toBe(true);
    expect(needsEngineSwap({ local_only: false })).toBe(false);
  });
  it("goes loading, then done", () => {
    let s = applySwap({}, "requested", ev);
    expect(swapText("stt", s.stt!)).toBe("Speech recognition: loading whisper-base…");
    s = applySwap(s, "done", ev);
    expect(swapText("stt", s.stt!)).toBe("Speech recognition: whisper-base ready");
    expect(s.tts).toBeUndefined();
  });
  it("failure shows the model id and, for a missing file, the expected path", () => {
    const s = applySwap({}, "failed", { ...ev, error: "model_missing", path: "C:\\m\\whisper-base.bin" });
    const t = swapText("stt", s.stt!);
    expect(t).toContain("whisper-base");
    expect(t).toContain("Expected at C:\\m\\whisper-base.bin");
    const o = applySwap({}, "failed", { ...ev, slot: "tts", error: "load_failed" });
    expect(swapText("tts", o.tts!)).toBe("Speech synthesis: whisper-base could not load (load_failed)");
  });
  it("maps command error tokens", () => {
    expect(swapErrorText("loader_unavailable")).toContain("not built");
    expect(swapErrorText("invalid_model")).toContain("no valid");
    expect(swapErrorText("zzz")).toBe("zzz");
  });
});
