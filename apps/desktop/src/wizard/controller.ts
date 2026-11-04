// The wizard's client state, free of React and Tauri so it can be unit-tested with a fake api.
// Loading and button presses (Next, Back, Skip, Retry) never call a probe: each probe method
// below is called only from the click handler of its own button.
import type { MicProbeDto } from "../ipc/MicProbeDto";
import type { WizardEvent } from "../ipc/WizardEvent";
import type { WizardView } from "../ipc/WizardView";
import { wizardErrorText } from "./logic";

export interface WizardApi {
  state(): Promise<WizardView>;
  event(e: WizardEvent): Promise<WizardView>;
  probeMachine(): Promise<WizardView>;
  probeMic(): Promise<MicProbeDto>;
  probeOllama(): Promise<WizardView>;
}

export interface ControllerState {
  view: WizardView | null;
  /** A probe or event is in flight. */
  busy: boolean;
  /** Fixed-token text for the last failure. */
  error: string | null;
  /** The last microphone reading (a level and a token, nothing else). */
  mic: MicProbeDto | null;
}

export class WizardController {
  private st: ControllerState = { view: null, busy: false, error: null, mic: null };
  private listeners = new Set<() => void>();

  constructor(private readonly api: WizardApi) {}

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  getState = (): ControllerState => this.st;

  private set(p: Partial<ControllerState>) {
    this.st = { ...this.st, ...p };
    for (const l of this.listeners) l();
  }

  private async run(f: () => Promise<WizardView>) {
    this.set({ busy: true, error: null });
    try {
      this.set({ view: await f(), busy: false });
    } catch (e) {
      this.set({ busy: false, error: wizardErrorText(e) });
    }
  }

  /** Read the saved progress. Runs no probe. */
  load = () => this.run(() => this.api.state());

  /** A button press. Runs no probe. */
  send = (e: WizardEvent) => {
    this.set({ mic: null });
    return this.run(() => this.api.event(e));
  };

  /** "Check my computer" click. */
  checkMachine = () => this.run(() => this.api.probeMachine());

  /** "Check" click on the Ollama step. */
  checkOllama = () => this.run(() => this.api.probeOllama());

  /** "Test microphone" click. */
  testMic = async () => {
    this.set({ busy: true, error: null });
    try {
      const mic = await this.api.probeMic();
      this.set({ busy: false, mic, view: mic.view });
    } catch (e) {
      this.set({ busy: false, error: wizardErrorText(e) });
    }
  };
}
