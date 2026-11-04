import { describe, expect, it } from "vitest";
import type { Settings } from "../ipc/Settings";
import type { SettingsIssue } from "../ipc/SettingsIssue";
import type { AgentTaskDto } from "../ipc/AgentTaskDto";
import {
  AGENT_ENABLED_NOTE,
  AGENT_MODE_NOTE,
  OBSERVE_MODE_NOTE,
  agentOutcomeText,
  agentReasonText,
  agentStartErrorText,
  agentStatusText,
  DEFAULT_HOTKEY,
  LOCAL_ONLY_OFF_NOTE,
  applySwap,
  buildPatch,
  cloudMissing,
  hotkeyHint,
  hotkeyProblem,
  killHotkeyProblem,
  DEFAULT_KILL_HOTKEY,
  issueText,
  shownValue,
  visionNote,
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
  kill_hotkey: "Ctrl+Alt+Shift+K",
  has_api_key: false,
  provider: "Ollama",
  anthropic_model: "",
  models_dir: "",
  default_models_dir: "C:\\Users\\me\\AppData\\models",
  observe_mode: true,
  agent_enabled: false,
  agent_mode: false,
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

describe("models folder in the patch", () => {
  it("sends a trimmed folder only when changed, and empty to reset", () => {
    expect(buildPatch(base, { ...base, models_dir: " D:\\models " })).toEqual({
      models_dir: "D:\\models",
    });
    const cur = { ...base, models_dir: "D:\\models" };
    expect(buildPatch(cur, { ...cur, models_dir: "" })).toEqual({ models_dir: "" });
    expect(isEmptyPatch(buildPatch(cur, { ...cur }))).toBe(true);
    expect(isEmptyPatch(buildPatch(base, { ...base, default_models_dir: "elsewhere" }))).toBe(true);
  });
  it("a changed folder (from the core's answer) needs an engine swap", () => {
    expect(needsEngineSwap({ models_dir: "D:\\m" }, true)).toBe(true);
    expect(needsEngineSwap({ models_dir: "D:\\m" }, false)).toBe(false);
    expect(needsEngineSwap({ profile: "low-memory" }, false)).toBe(true);
  });
});

describe("hotkeyProblem", () => {
  it("accepts chords with Ctrl, Alt or Super", () => {
    for (const ok of ["Ctrl+Alt+Space", "alt+k", "Super+1", "cmd+shift+f12", " Ctrl + A "]) {
      expect(hotkeyProblem(ok), ok).toBeNull();
    }
  });
  it("explains why a chord is invalid", () => {
    expect(hotkeyProblem("Space")).toBe("needs_modifier");
    expect(hotkeyProblem("Shift+A")).toBe("needs_modifier");
    expect(hotkeyProblem("Ctrl")).toBe("invalid_format");
    expect(hotkeyProblem("")).toBe("invalid_format");
    expect(hotkeyProblem("Ctrl+Ctrl+A")).toBe("invalid_format");
    expect(hotkeyProblem("Ctrl+F25")).toBe("invalid_format");
    expect(hotkeyProblem("Ctrl+A+B")).toBe("invalid_format");
    expect(hotkeyProblem("Ctrl+\u202eA")).toBe("control_chars");
    expect(hotkeyProblem("Ctrl+A\u0000")).toBe("control_chars");
    expect(hotkeyProblem(`Ctrl+${"a".repeat(10_240)}`)).toBe("too_long");
    expect(hotkeyHint("needs_modifier")).toMatch(/Ctrl\/Alt\/Super/);
    expect(hotkeyHint("invalid_format")).toMatch(/for example Ctrl\+Alt\+Space/);
  });
});

describe("issue rendering", () => {
  const issue = (field: SettingsIssue["field"], code: SettingsIssue["code"]): SettingsIssue => ({
    field,
    code,
  });
  it("says what was not saved and why, with the user's own text", () => {
    expect(issueText(issue("hotkey", "needs_modifier"), { hotkey: "Space" })).toBe(
      `Your hotkey 'Space' was not saved: needs Ctrl/Alt/Super. Using the default (${DEFAULT_HOTKEY}).`,
    );
    const m = issueText(issue("models_dir", "traversal"), { models_dir: "C:\\a\\..\\b" });
    expect(m).toContain("C:\\a\\..\\b");
    expect(m).toContain("must not contain '..'");
    expect(m).toContain("default folder");
    expect(issueText(issue("anthropic_model", "invalid_format"), { anthropic_model: "a b" })).toMatch(
      /Cloud stays unavailable/,
    );
    expect(issueText(issue("profile", "unknown"))).toMatch(/default profile/);
    expect(issueText(issue("file", "corrupt"))).toMatch(/default settings/);
  });
  it("does not invent text when the attempted value is unknown", () => {
    expect(issueText(issue("hotkey", "unavailable"))).toMatch(/^The saved hotkey was not saved/);
    expect(issueText(issue("models_dir", "not_a_directory"))).not.toContain("'");
  });
  it("shows hostile input safely", () => {
    expect(shownValue("a\u202eb\u0000c\nd")).toBe("abcd");
    expect(shownValue("x".repeat(10_240))).toHaveLength(41);
    const t = issueText(issue("hotkey", "control_chars"), { hotkey: "\u202egnp" });
    expect(t).not.toContain("\u202e");
  });
});

describe("explanations", () => {
  it("local-only off still promises the preview", () => {
    expect(LOCAL_ONLY_OFF_NOTE).toMatch(/preview|shows you exactly/);
    expect(LOCAL_ONLY_OFF_NOTE).toMatch(/Send/);
  });
  it("vision note only for low-memory", () => {
    expect(visionNote("low-memory")).toMatch(/Off/);
    expect(visionNote("default")).toBeNull();
  });
  it("lists what cloud still needs", () => {
    expect(cloudMissing(base)).toEqual([
      "choose Anthropic as the language model",
      "turn off Local only",
      "set a model id",
      "store an API key",
    ]);
    expect(
      cloudMissing({
        ...base,
        provider: "Anthropic",
        local_only: false,
        anthropic_model: "m",
        has_api_key: true,
      }),
    ).toEqual([]);
  });
});

describe("killHotkeyProblem", () => {
  const ptt = "Ctrl+Alt+Space";
  it("accepts chords with two modifiers that differ from push-to-talk", () => {
    for (const ok of ["Ctrl+Alt+Shift+K", "alt+shift+f9", "Ctrl+Alt+K", "cmd+shift+x"]) {
      expect(killHotkeyProblem(ok, ptt), ok).toBeNull();
    }
  });
  it("rejects what the core rejects", () => {
    expect(killHotkeyProblem("Ctrl+K", ptt)).toBe("needs_two_modifiers");
    expect(killHotkeyProblem("Shift+K", ptt)).toBe("needs_modifier");
    expect(killHotkeyProblem("space+alt+ctrl", ptt)).toBe("duplicate");
    expect(killHotkeyProblem("Ctrl+Alt+\u202eK", ptt)).toBe("control_chars");
    expect(killHotkeyProblem("", ptt)).toBe("invalid_format");
  });
  it("patches and explains", () => {
    expect(buildPatch(base, { ...base, kill_hotkey: " Alt+Shift+F9 " })).toEqual({
      kill_hotkey: "Alt+Shift+F9",
    });
    expect(
      issueText({ field: "kill_hotkey", code: "duplicate" }, { kill_hotkey: "Ctrl+Alt+Space" }),
    ).toBe(
      `Your stop shortcut 'Ctrl+Alt+Space' was not saved: it is the same as the other shortcut. Using the default (${DEFAULT_KILL_HOTKEY}).`,
    );
    const unavailable = issueText(
      { field: "kill_hotkey", code: "unavailable" },
      { kill_hotkey: "Ctrl+Alt+Shift+J" },
    );
    expect(unavailable).toBe(
      "Your stop shortcut 'Ctrl+Alt+Shift+J' was not saved: the chord could not be registered; the previous one is still active.",
    );
    expect(unavailable).not.toContain("Using the default");
  });
});

describe("agent settings", () => {
  it("defaults: observe mode on, agent off, voice routing off", () => {
    expect(base.observe_mode).toBe(true);
    expect(base.agent_enabled).toBe(false);
    expect(base.agent_mode).toBe(false);
  });
  it("sends only the changed agent fields", () => {
    expect(buildPatch(base, { ...base, observe_mode: false })).toEqual({ observe_mode: false });
    expect(buildPatch(base, { ...base, agent_enabled: true })).toEqual({ agent_enabled: true });
    expect(buildPatch(base, { ...base, agent_enabled: true, agent_mode: true })).toEqual({
      agent_enabled: true,
      agent_mode: true,
    });
  });
  it("never sends agent_mode on without the agent enabled", () => {
    expect(isEmptyPatch(buildPatch(base, { ...base, agent_mode: true }))).toBe(true);
    const on: Settings = { ...base, agent_enabled: true, agent_mode: true };
    expect(buildPatch(on, { ...on, agent_enabled: false })).toEqual({
      agent_enabled: false,
      agent_mode: false,
    });
  });
  it("says observe mode is a dry run first, and that turning it off keeps every check", () => {
    expect(OBSERVE_MODE_NOTE).toMatch(/dry run/);
    expect(OBSERVE_MODE_NOTE).toMatch(/Run it for real/);
    expect(OBSERVE_MODE_NOTE).toMatch(/every approval/);
    expect(AGENT_ENABLED_NOTE).toMatch(/Off by default/);
    expect(AGENT_MODE_NOTE).toMatch(/Off by default/);
  });
  it("maps fixed tokens to text", () => {
    expect(agentReasonText(null)).toBe("Agent: ready");
    expect(agentReasonText("setting_off")).toMatch(/enable/);
    expect(agentReasonText("not_ready")).toMatch(/activity log/);
    expect(agentReasonText("no_runtime")).toMatch(/not available/);
    expect(agentReasonText("???")).toBe("Agent: not available");
    expect(agentStatusText(null)).toMatch(/checking/);
    expect(
      agentStatusText({ ready: true, running: true, observe_mode: true, agent_mode: false, reason: null }),
    ).toMatch(/running/);
    expect(agentStartErrorText("agent_disabled")).toMatch(/not enabled/);
    expect(agentStartErrorText("busy")).toMatch(/already running/);
    expect(agentStartErrorText("bad_request")).toMatch(/Type what/);
    expect(agentStartErrorText("agent_unavailable")).toMatch(/not available/);
    expect(agentStartErrorText("forbidden")).toBe("forbidden");
  });
  it("describes how a task ended from tokens and counts", () => {
    const t = (o: Partial<AgentTaskDto>): AgentTaskDto => ({
      task: 1,
      outcome: "completed",
      detail: "ok",
      dry_run: true,
      would_do: 2,
      steps_done: 2,
      executed: 2,
      live: true,
      answers: [],
      ...o,
    });
    expect(agentOutcomeText(t({}))).toBe("Done (2 steps done, 2 actions)");
    expect(agentOutcomeText(t({ outcome: "denied", live: false }))).toMatch(/Dry run only/);
    expect(agentOutcomeText(t({ outcome: "killed" }))).toBe("Stopped");
    expect(agentOutcomeText(t({ outcome: "stopped", detail: "runaway" }))).toMatch(/runaway/);
    expect(agentOutcomeText(t({ outcome: "refused", detail: "busy" }))).toBe("Not started (busy)");
    expect(agentOutcomeText(t({ outcome: "blocked", detail: "outside_plan" }))).toMatch(
      /outside_plan/,
    );
  });
});
