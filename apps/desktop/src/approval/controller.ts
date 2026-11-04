// The approval window's state machine, free of React and Tauri so it can be unit-tested.
// It can only RESPOND to approvals core created: there is no way in here to make one.
import type { ApprovalClosedDto } from "../ipc/ApprovalClosedDto";
import type { ApprovalDecisionDto } from "../ipc/ApprovalDecisionDto";
import type { ApprovalRequestDto } from "../ipc/ApprovalRequestDto";
import { guardLeft } from "./logic";

export interface ApprovalApi {
  displayed(id: string): Promise<void>;
  respond(id: string, decision: ApprovalDecisionDto, echoedDigest: string): Promise<void>;
  kill(): Promise<void>;
}

export interface ViewState {
  /** Waiting approvals, oldest first; the first one is on screen. */
  queue: ApprovalRequestDto[];
  /** When core acknowledged the first one as displayed (client clock); null until then. */
  shownAt: number | null;
  busy: boolean;
  /** A fixed token from core, or "ack_failed". Shown as text. */
  error: string | null;
  /** The last thing that ended the view ("killed", ...), shown once the queue is empty. */
  ended: string | null;
}

export interface KeyLike {
  key: string;
  ctrlKey?: boolean;
  shiftKey?: boolean;
  preventDefault(): void;
}

/** Errors that mean the request no longer exists in core. */
const GONE = new Set(["unknown_id", "expired", "killed"]);

export class ApprovalController {
  private state: ViewState = { queue: [], shownAt: null, busy: false, error: null, ended: null };
  private listeners = new Set<() => void>();
  private ackFor: string | null = null;

  constructor(
    private readonly api: ApprovalApi,
    private readonly now: () => number = () => Date.now(),
  ) {}

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  getState = (): ViewState => this.state;

  private set(patch: Partial<ViewState>) {
    this.state = { ...this.state, ...patch };
    this.listeners.forEach((l) => l());
  }

  current(): ApprovalRequestDto | null {
    return this.state.queue[0] ?? null;
  }

  /** A request from core (event or `approval_ready`). Duplicates by id are ignored. */
  onRequest(dto: ApprovalRequestDto) {
    if (this.state.queue.some((q) => q.id === dto.id)) return;
    this.set({ queue: [...this.state.queue, dto], ended: null });
    void this.display();
  }

  /** Core ended a request (answered, expired or killed): drop it; killed closes the view. */
  onClosed(c: ApprovalClosedDto) {
    const wasCurrent = this.current()?.id === c.id;
    const queue = this.state.queue.filter((q) => q.id !== c.id);
    if (queue.length === this.state.queue.length) return;
    this.set({
      queue,
      shownAt: wasCurrent ? null : this.state.shownAt,
      busy: wasCurrent ? false : this.state.busy,
      error: wasCurrent ? null : this.state.error,
      ended: queue.length === 0 ? c.outcome.toLowerCase() : null,
    });
    if (wasCurrent) {
      this.ackFor = null;
      void this.display();
    }
  }

  /** Tell core the current request is on screen; Approve unlocks 500 ms after this returns. */
  async display(): Promise<void> {
    const cur = this.current();
    if (!cur || this.ackFor === cur.id) return;
    this.ackFor = cur.id;
    try {
      await this.api.displayed(cur.id);
      if (this.current()?.id === cur.id) this.set({ shownAt: this.now(), error: null });
    } catch {
      // No acknowledgement means Approve stays locked (fail closed); Deny still works.
      if (this.current()?.id === cur.id) this.set({ error: "ack_failed" });
    }
  }

  guardLeft(now: number = this.now()): number | null {
    return guardLeft(this.state.shownAt, now);
  }

  approveEnabled(now: number = this.now()): boolean {
    return this.current() !== null && !this.state.busy && this.guardLeft(now) === 0;
  }

  /** `synthetic` = the click came from the keyboard (Enter/Space on a focused button). */
  async approve(opts: { synthetic?: boolean } = {}): Promise<void> {
    if (opts.synthetic) return;
    const cur = this.current();
    if (!cur || !this.approveEnabled()) return;
    await this.answer(cur, "Approve");
  }

  /** Always allowed (unless an answer is in flight). */
  async deny(): Promise<void> {
    const cur = this.current();
    if (!cur) return;
    await this.answer(cur, "Deny");
  }

  private async answer(cur: ApprovalRequestDto, decision: ApprovalDecisionDto) {
    if (this.state.busy) return;
    this.set({ busy: true, error: null });
    try {
      // The digest shown is echoed back exactly as core sent it.
      await this.api.respond(cur.id, decision, cur.digest_hex);
      this.onClosed({ id: cur.id, outcome: decision === "Approve" ? "Approved" : "Denied" });
    } catch (e) {
      const code = typeof e === "string" ? e : "failed";
      if (GONE.has(code)) {
        this.onClosed({ id: cur.id, outcome: code === "expired" ? "Expired" : "Killed" });
      } else {
        this.set({ busy: false, error: /^[a-z_]{1,32}$/.test(code) ? code : "failed" });
      }
    }
  }

  /** STOP. Closes the view at once; core resolves everything pending as Killed. */
  async kill(): Promise<void> {
    this.set({ queue: [], shownAt: null, busy: false, error: null, ended: "killed" });
    this.ackFor = null;
    try {
      await this.api.kill();
    } catch {
      // The kill hotkey and tray item still work; nothing to show.
    }
  }

  /**
   * Keyboard: Esc = Deny, Ctrl+Shift+A = Approve (still behind the guard), Enter and Space do
   * nothing (they must never approve).
   */
  handleKey(e: KeyLike): void {
    if (e.key === "Escape") {
      e.preventDefault();
      void this.deny();
    } else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "a") {
      e.preventDefault();
      void this.approve();
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
    }
  }
}
