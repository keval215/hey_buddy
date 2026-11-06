import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { Settings } from "../ipc/Settings";
import type { WizardStep } from "../ipc/WizardStep";
import type { WizardView } from "../ipc/WizardView";
import type { ControllerState } from "./controller";
import { Wizard, WizardViewPanel, type Actions } from "./Wizard";
import source from "./Wizard.tsx?raw";
import controllerSource from "./controller.ts?raw";
import { STEPS } from "./logic";

const settings = {
  local_only: true,
  observe_mode: true,
  agent_enabled: false,
  agent_mode: false,
  diagnostic_logs: false,
  vision_mode: "Auto",
  profile: "default",
  hotkey: "Ctrl+Alt+Space",
} as Settings;

const view = (step: WizardStep): WizardView => ({
  state: {
    step,
    completed: false,
    skipped_steps: ["Microphone"],
    recommended_profile: "low-memory",
    machine: { ram_mb: 8192, gpu_name: "Test GPU", vram_mb: 2048 },
    ollama: { running: false, models: [{ tag: "qwen3:4b", present: false }] },
    mic_ok: null,
    hotkey_ok: null,
  },
  profile: "default",
  model_ids: ["whisper-small"],
  ollama_tags: ["qwen3:4b"],
});

function actions(): Actions {
  return {
    send: vi.fn(),
    checkMachine: vi.fn(),
    checkOllama: vi.fn(),
    testMic: vi.fn(),
    useProfile: vi.fn(),
    applyHotkey: vi.fn(),
  };
}

function render(step: WizardStep, over: Partial<ControllerState> = {}) {
  const ctl: ControllerState = { view: view(step), busy: false, error: null, mic: null, ...over };
  const a = actions();
  const html = renderToStaticMarkup(
    <WizardViewPanel ctl={ctl} settings={settings} denyCount={12} hotkeyMessage={null} actions={a} />,
  );
  return { html, a };
}

describe("wizard view", () => {
  it("renders every step with the stepper, Back/Skip/Next and a labelled heading", () => {
    for (const { step } of STEPS) {
      if (step === "Models") continue; // embeds the models table (loads through effects)
      const { html } = render(step);
      expect(html).toContain('aria-label="Setup steps"');
      expect(html).toContain('aria-current="step"');
      expect(html).toContain("Back");
      expect(html).toContain('aria-labelledby="wizard-heading"');
    }
  });

  it("the Welcome step shows the privacy summary", () => {
    const { html } = render("Welcome");
    expect(html).toContain("Local first");
    expect(html).toContain("until you have seen exactly what would be sent");
  });

  it("Skip is disabled on the safety step and its note says the defaults stay on", () => {
    const { html } = render("SafetyDefaults");
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*aria-describedby="wizard-skip-note"[^>]*>Skip/);
    expect(html).toContain("cannot be skipped");
    expect(html).toContain("stay on either way");
    expect(html).toContain("Keep these safe settings");
    expect(html).toContain("12 entries");
  });

  it("Skip is enabled elsewhere and says skipping is safe", () => {
    const { html } = render("Ollama");
    expect(html).not.toMatch(/<button[^>]*disabled=""[^>]*aria-describedby="wizard-skip-note"/);
    expect(html).toContain("Skipping is fine");
  });

  it("probe buttons exist but nothing is called by rendering", () => {
    for (const [step, label] of [
      ["MachineCheck", "Check again"],
      ["Microphone", "Test microphone"],
      ["Ollama", "Check again"],
    ] as const) {
      const { html, a } = render(step);
      expect(html).toContain(label);
      expect(a.checkMachine).not.toHaveBeenCalled();
      expect(a.testMic).not.toHaveBeenCalled();
      expect(a.checkOllama).not.toHaveBeenCalled();
      expect(a.send).not.toHaveBeenCalled();
    }
  });

  it("shows install instructions as text when Ollama is not running, with no link element", () => {
    const { html } = render("Ollama");
    expect(html).toContain("never installs or starts it");
    expect(html).toContain("ollama.com");
    expect(html).not.toContain("<a ");
    expect(html).not.toContain("href=");
  });

  it("the machine step shows the plain-text result and the recommendation", () => {
    const { html } = render("MachineCheck");
    expect(html).toContain("Graphics card: Test GPU");
    expect(html).toContain("Recommended profile: low-memory");
  });

  it("the microphone step shows a level meter and text, no audio element", () => {
    const { html } = render("Microphone", {
      mic: { peak: 0.5, rms: 0.2, error: null, view: view("Microphone") },
    });
    expect(html).toContain("level 50%");
    expect(html).not.toContain("<audio");
  });

  it("the hotkey step offers Save and test", () => {
    expect(render("Hotkey").html).toContain("Save and test this hotkey");
  });

  it("an unreadable view shows a message, not a crash", () => {
    const html = renderToStaticMarkup(
      <WizardViewPanel
        ctl={{ view: null, busy: false, error: "Something went wrong", mic: null }}
        settings={settings}
        denyCount={null}
        hotkeyMessage={null}
        actions={actions()}
      />,
    );
    expect(html).toContain("Something went wrong");
  });

  it("mounting the container renders the loading state without calling anything", () => {
    // Static rendering runs no effects: the container must still render.
    const html = renderToStaticMarkup(
      <Wizard settings={settings} onSettings={vi.fn()} onClose={vi.fn()} />,
    );
    expect(html).toContain("Loading");
  });
});

/** The text inside each `useEffect(...)` call (balanced parentheses). */
function effectBodies(src: string): string[] {
  const out: string[] = [];
  let at = src.indexOf("useEffect(");
  while (at !== -1) {
    let depth = 0;
    let i = at + "useEffect".length;
    do {
      if (src[i] === "(") depth++;
      if (src[i] === ")") depth--;
      i++;
    } while (depth > 0 && i < src.length);
    out.push(src.slice(at, i));
    at = src.indexOf("useEffect(", i);
  }
  return out;
}

describe("source guarantees", () => {
  it("never uses dangerouslySetInnerHTML or raw html", () => {
    for (const s of [source, controllerSource]) {
      expect(s).not.toContain("dangerouslySetInnerHTML");
      expect(s).not.toContain("innerHTML");
    }
  });

  it("probe commands are reached only from click handlers, never from effects", () => {
    // Every use of a probe action sits in an onClick; the effects only call load / getDenyList.
    const effects = effectBodies(source).join("\n");
    expect(effects).toContain("ctl.load()");
    expect(effects).not.toMatch(/probe|checkMachine|checkOllama|testMic/i);
    for (const name of ["checkMachine", "checkOllama", "testMic"]) {
      const uses = [...source.matchAll(new RegExp(`actions\\.${name}`, "g"))].length;
      const inClick = [...source.matchAll(new RegExp(`onClick=\\{actions\\.${name}\\}`, "g"))].length;
      expect(uses).toBe(inClick);
      expect(uses).toBe(1);
    }
    // The raw api probe calls exist only in the adapter object and nowhere in a component body.
    expect(source.match(/api\.wizardProbe/g)?.length).toBe(3);
    // In the controller each probe is called exactly once, inside its own method.
    expect(controllerSource.match(/this\.api\.probe/g)?.length).toBe(3);
    // load() is the only thing the container calls on mount.
    expect(source).toContain("useEffect(() => void ctl.load(), [ctl]);");
  });
});
