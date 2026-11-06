import { describe, expect, it, vi } from "vitest";
import type { MicProbeDto } from "../ipc/MicProbeDto";
import type { WizardEvent } from "../ipc/WizardEvent";
import type { WizardView } from "../ipc/WizardView";
import { WizardController, type WizardApi } from "./controller";

const view = (step: WizardView["state"]["step"] = "Welcome"): WizardView => ({
  state: {
    step,
    completed: false,
    skipped_steps: [],
    recommended_profile: null,
    machine: null,
    ollama: null,
    mic_ok: null,
    hotkey_ok: null,
  },
  profile: "default",
  model_ids: [],
  ollama_tags: [],
});

function fake() {
  const mic: MicProbeDto = { peak: 0.3, rms: 0.1, error: null, view: view("Microphone") };
  const api: WizardApi = {
    state: vi.fn(async () => view()),
    event: vi.fn(async () => view("MachineCheck")),
    probeMachine: vi.fn(async () => view("MachineCheck")),
    probeMic: vi.fn(async () => mic),
    probeOllama: vi.fn(async () => view("Ollama")),
  };
  return api;
}

const probes = (a: WizardApi) =>
  [a.probeMachine, a.probeMic, a.probeOllama].map((f) => (f as ReturnType<typeof vi.fn>).mock.calls.length);

describe("wizard controller", () => {
  it("loading reads the saved state and calls no probe", async () => {
    const a = fake();
    const c = new WizardController(a);
    await c.load();
    expect(a.state).toHaveBeenCalledTimes(1);
    expect(probes(a)).toEqual([0, 0, 0]);
    expect(c.getState().view?.state.step).toBe("Welcome");
  });

  it("every button event and step change calls no probe", async () => {
    const a = fake();
    const c = new WizardController(a);
    await c.load();
    const evs: WizardEvent[] = ["Next", "Back", "Skip", "Retry", "Finish", "Rerun", { HotkeyResult: true }];
    for (let i = 0; i < 3; i++) for (const e of evs) await c.send(e);
    expect(a.event).toHaveBeenCalledTimes(21);
    expect(probes(a)).toEqual([0, 0, 0]);
  });

  it("each probe runs once per explicit call and nothing else", async () => {
    const a = fake();
    const c = new WizardController(a);
    await c.checkMachine();
    expect(probes(a)).toEqual([1, 0, 0]);
    await c.testMic();
    expect(probes(a)).toEqual([1, 1, 0]);
    expect(c.getState().mic?.peak).toBe(0.3);
    await c.checkOllama();
    expect(probes(a)).toEqual([1, 1, 1]);
  });

  it("a button press clears the microphone reading", async () => {
    const a = fake();
    const c = new WizardController(a);
    await c.testMic();
    await c.send("Next");
    expect(c.getState().mic).toBeNull();
  });

  it("failures become fixed text and the state stays usable", async () => {
    const a = fake();
    (a.event as ReturnType<typeof vi.fn>).mockRejectedValueOnce("io_error");
    const c = new WizardController(a);
    await c.load();
    await c.send("Next");
    expect(c.getState().error).toContain("could not be saved");
    expect(c.getState().busy).toBe(false);
    await c.send("Next");
    expect(c.getState().error).toBeNull();
  });

  it("notifies subscribers", async () => {
    const a = fake();
    const c = new WizardController(a);
    const l = vi.fn();
    const un = c.subscribe(l);
    await c.load();
    expect(l).toHaveBeenCalled();
    un();
    l.mockClear();
    await c.load();
    expect(l).not.toHaveBeenCalled();
  });
});
