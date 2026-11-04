import { describe, expect, it } from "vitest";
import type { MicProbeDto } from "../ipc/MicProbeDto";
import type { Settings } from "../ipc/Settings";
import type { WizardState } from "../ipc/WizardState";
import type { WizardStep } from "../ipc/WizardStep";
import {
  PRIVACY_POINTS,
  STEPS,
  canBack,
  canSkip,
  hotkeyAccepted,
  machineLines,
  meterPercent,
  micText,
  modelsForProfile,
  nextLabel,
  ollamaLines,
  recommendationText,
  safetyChanges,
  safetyIntro,
  safetyRows,
  shouldAutoShow,
  skipNote,
  stepNumber,
  wizardErrorText,
} from "./logic";
import type { ModelRow } from "../ipc/ModelRow";

const fresh: WizardState = {
  step: "Welcome",
  completed: false,
  skipped_steps: [],
  recommended_profile: null,
  machine: null,
  ollama: null,
  mic_ok: null,
  hotkey_ok: null,
};
const safe = {
  local_only: true,
  observe_mode: true,
  agent_enabled: false,
  agent_mode: false,
  vision_mode: "Auto",
  profile: "default",
} as Settings;

describe("steps", () => {
  it("are the eight steps in order", () => {
    expect(STEPS.map((s) => s.step)).toEqual([
      "Welcome",
      "MachineCheck",
      "Microphone",
      "Ollama",
      "Models",
      "Hotkey",
      "SafetyDefaults",
      "Done",
    ]);
    expect(stepNumber("Welcome")).toBe(1);
    expect(stepNumber("Done")).toBe(8);
  });

  it("can be skipped except the safety confirm (and Done), and the note says defaults stay safe", () => {
    for (const { step } of STEPS) {
      const ok = step !== "SafetyDefaults" && step !== "Done";
      expect(canSkip(step)).toBe(ok);
    }
    expect(skipNote("Models")).toContain("safe settings stay on");
    expect(skipNote("SafetyDefaults")).toContain("cannot be skipped");
    expect(skipNote("SafetyDefaults")).toContain("stay on either way");
  });

  it("labels the confirm so it can only mean keeping the safe settings", () => {
    expect(nextLabel("SafetyDefaults")).toBe("Keep these safe settings");
    expect(nextLabel("Done")).toBe("Finish");
    expect(nextLabel("Models")).toBe("Next");
  });

  it("back is unavailable at the start and after finishing", () => {
    expect(canBack(fresh)).toBe(false);
    expect(canBack({ ...fresh, step: "Models" })).toBe(true);
    expect(canBack({ ...fresh, step: "Done", completed: true })).toBe(false);
  });

  it("auto-shows until completed", () => {
    expect(shouldAutoShow(null)).toBe(false);
    expect(shouldAutoShow(fresh)).toBe(true);
    expect(shouldAutoShow({ ...fresh, step: "Done", completed: true })).toBe(false);
  });
});

describe("texts", () => {
  it("the privacy summary covers local, cloud, preview, agent and the no-surprise promise", () => {
    const all = PRIVACY_POINTS.join(" ").toLowerCase();
    for (const w of ["local", "cloud", "send", "agent", "until you click"]) expect(all).toContain(w);
  });

  it("machine lines never claim what is unknown", () => {
    expect(machineLines({ ram_mb: 16384, gpu_name: "RTX 2050", vram_mb: 4096 })).toEqual([
      "Memory (RAM): 16 GB",
      "Graphics card: RTX 2050",
      "Graphics memory: 4.0 GB",
    ]);
    expect(machineLines({ ram_mb: null, gpu_name: null, vram_mb: null })).toEqual([
      "Memory (RAM): could not be read",
      "Graphics card: not found",
      "Graphics memory: unknown",
    ]);
  });

  it("recommendation explains unknown numbers and the current choice", () => {
    const st: WizardState = {
      ...fresh,
      step: "MachineCheck",
      machine: { ram_mb: null, gpu_name: null, vram_mb: null },
      recommended_profile: "low-memory",
    };
    expect(recommendationText(st, "default")).toContain("smaller choice");
    const ok: WizardState = {
      ...st,
      machine: { ram_mb: 16384, gpu_name: "G", vram_mb: 8192 },
      recommended_profile: "default",
    };
    expect(recommendationText(ok, "default")).toContain("already selected");
    expect(recommendationText(ok, "low-memory")).toContain("You are using low-memory");
    expect(recommendationText(fresh, "default")).toBeNull();
  });

  it("ollama lines: not running covers not installed; missing models show the pull command", () => {
    expect(ollamaLines({ running: false, models: [] })[0]).toContain("may not be installed");
    const l = ollamaLines({
      running: true,
      models: [
        { tag: "qwen3:4b", present: true },
        { tag: "qwen3-vl:2b", present: false },
      ],
    });
    expect(l).toEqual([
      "Ollama is running.",
      "Model qwen3:4b: installed",
      "Model qwen3-vl:2b: not installed yet (ollama pull qwen3-vl:2b)",
    ]);
  });

  it("microphone texts and meter", () => {
    const dto = (error: string | null, mic_ok: boolean | null): MicProbeDto => ({
      peak: 0.3,
      rms: 0.1,
      error,
      view: { state: { ...fresh, mic_ok }, profile: "default", model_ids: [], ollama_tags: [] },
    });
    expect(micText(dto(null, true))).toContain("works");
    expect(micText(dto(null, false))).toContain("almost no sound");
    expect(micText(dto("mic_timeout", false))).toContain("5 seconds");
    expect(micText(dto("mic_unavailable", false))).toContain("not available");
    expect(micText(dto("weird", false))).toBe("The microphone test failed.");
    expect(meterPercent(0.5)).toBe(50);
    expect(meterPercent(-1)).toBe(0);
    expect(meterPercent(7)).toBe(100);
    expect(meterPercent(Number.NaN)).toBe(0);
  });

  it("error tokens map to text and unknown tokens do not leak objects", () => {
    expect(wizardErrorText("wrong_step")).toContain("different step");
    expect(wizardErrorText({ x: 1 })).toBe("Something went wrong");
  });
});

describe("models and hotkey", () => {
  it("filters the table to the profile's ids", () => {
    const rows = [{ id: "a" }, { id: "b" }, { id: "c" }] as ModelRow[];
    expect(modelsForProfile(rows, ["c", "a"]).map((r) => r.id)).toEqual(["a", "c"]);
    expect(modelsForProfile(rows, [])).toEqual([]);
  });

  it("hotkey passes only with no hotkey issue AND a registered shortcut", () => {
    const res = (issues: { field: string; code: string }[]) =>
      ({ settings: safe, issues, models_dir_changed: false }) as never;
    const st = (registered: boolean) => ({ hotkey_registered: registered }) as never;
    expect(hotkeyAccepted(res([]), st(true))).toBe(true);
    expect(hotkeyAccepted(res([]), st(false))).toBe(false);
    expect(hotkeyAccepted(res([{ field: "hotkey", code: "unavailable" }]), st(true))).toBe(false);
    expect(hotkeyAccepted(res([{ field: "models_dir", code: "unknown" }]), st(true))).toBe(true);
  });
});

describe("safety rows", () => {
  it("are all safe for the defaults", () => {
    const rows = safetyRows(safe);
    expect(rows.every((r) => r.safe)).toBe(true);
    expect(safetyIntro(rows)).toContain("already the safe ones");
    expect(safetyIntro(rows)).toContain("never off");
  });

  it("flags every protection that is off and says the wizard only turns things on", () => {
    const bad = {
      ...safe,
      local_only: false,
      observe_mode: false,
      agent_enabled: true,
      agent_mode: true,
      vision_mode: "Always",
    } as Settings;
    const rows = safetyRows(bad);
    expect(safetyChanges(rows)).toBe(5);
    expect(safetyIntro(rows)).toContain("5 of these");
    expect(safetyIntro(rows)).toContain("only turn protections on");
  });

  it("low-memory shows vision as off and counts as safe", () => {
    const rows = safetyRows({ ...safe, profile: "low-memory", vision_mode: "Off" });
    expect(rows.find((r) => r.label === "Screen vision")?.now).toContain("off");
    expect(rows.every((r) => r.safe)).toBe(true);
  });
});

describe("step list typing", () => {
  it("covers every WizardStep value", () => {
    const all: WizardStep[] = STEPS.map((s) => s.step);
    expect(new Set(all).size).toBe(8);
  });
});
