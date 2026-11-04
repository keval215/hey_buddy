import { describe, expect, it, vi } from "vitest";
import type { ApprovalRequestDto } from "../ipc/ApprovalRequestDto";
import { ApprovalController, type ApprovalApi } from "./controller";
import { GUARD_MS } from "./logic";

const DIGEST = "ab".repeat(32);

function req(id: string, over: Partial<ApprovalRequestDto> = {}): ApprovalRequestDto {
  return {
    id,
    kind: "Step",
    digest_hex: DIGEST,
    summary: "Step 1: click (consequential) abababab",
    lines: [
      { index: 0, op: "click", target: "Save", text: null, tier: "Consequential", refused: false, ask_again: false },
    ],
    live: true,
    changed: false,
    timeout_secs: 30,
    ...over,
  };
}

function setup() {
  let t = 1_000;
  const api = {
    displayed: vi.fn<ApprovalApi["displayed"]>(() => Promise.resolve()),
    respond: vi.fn<ApprovalApi["respond"]>(() => Promise.resolve()),
    kill: vi.fn<ApprovalApi["kill"]>(() => Promise.resolve()),
  };
  const c = new ApprovalController(api, () => t);
  return { c, api, advance: (ms: number) => (t += ms), at: () => t };
}

const flush = () => new Promise((r) => setTimeout(r, 0));
const key = (key: string, extra: { ctrlKey?: boolean; shiftKey?: boolean } = {}) => ({
  key,
  ...extra,
  preventDefault: vi.fn(),
});

describe("input guard", () => {
  it("Approve is disabled until the display ack plus 500 ms; Deny is always enabled", async () => {
    const { c, api, advance } = setup();
    c.onRequest(req("a"));
    // Not acknowledged yet: locked.
    expect(c.approveEnabled()).toBe(false);
    expect(c.guardLeft()).toBeNull();
    await flush();
    expect(api.displayed).toHaveBeenCalledWith("a");
    expect(c.guardLeft()).toBe(GUARD_MS);
    expect(c.approveEnabled()).toBe(false);
    advance(GUARD_MS - 1);
    expect(c.approveEnabled()).toBe(false);
    await c.approve();
    expect(api.respond).not.toHaveBeenCalled();
    advance(1);
    expect(c.approveEnabled()).toBe(true);
    await c.approve();
    expect(api.respond).toHaveBeenCalledTimes(1);
  });

  it("a failed display ack keeps Approve locked but Deny works", async () => {
    const { c, api, advance } = setup();
    api.displayed.mockRejectedValueOnce("unknown_id");
    c.onRequest(req("a"));
    await flush();
    advance(10_000);
    expect(c.getState().error).toBe("ack_failed");
    expect(c.approveEnabled()).toBe(false);
    await c.deny();
    expect(api.respond).toHaveBeenCalledWith("a", "Deny", DIGEST);
  });

  it("a keyboard-synthesised click (Enter/Space on a button) never approves", async () => {
    const { c, api, advance } = setup();
    c.onRequest(req("a"));
    await flush();
    advance(GUARD_MS);
    await c.approve({ synthetic: true });
    expect(api.respond).not.toHaveBeenCalled();
    for (const k of ["Enter", " "]) {
      const e = key(k);
      c.handleKey(e);
      expect(e.preventDefault).toHaveBeenCalled();
    }
    expect(api.respond).not.toHaveBeenCalled();
  });
});

describe("keyboard", () => {
  it("Esc denies, even inside the guard", async () => {
    const { c, api } = setup();
    c.onRequest(req("a"));
    await flush();
    c.handleKey(key("Escape"));
    await flush();
    expect(api.respond).toHaveBeenCalledWith("a", "Deny", DIGEST);
    expect(c.current()).toBeNull();
  });

  it("Ctrl+Shift+A approves only after the guard; plain A does nothing", async () => {
    const { c, api, advance } = setup();
    c.onRequest(req("a"));
    await flush();
    c.handleKey(key("A", { ctrlKey: true, shiftKey: true }));
    await flush();
    expect(api.respond).not.toHaveBeenCalled();
    c.handleKey(key("a"));
    advance(GUARD_MS);
    c.handleKey(key("a"));
    await flush();
    expect(api.respond).not.toHaveBeenCalled();
    c.handleKey(key("A", { ctrlKey: true, shiftKey: true }));
    await flush();
    expect(api.respond).toHaveBeenCalledWith("a", "Approve", DIGEST);
  });
});

describe("digest echo", () => {
  it("sends back exactly the digest core sent, for both answers", async () => {
    const { c, api, advance } = setup();
    c.onRequest(req("a", { digest_hex: "0f".repeat(32) }));
    await flush();
    advance(GUARD_MS);
    await c.approve();
    expect(api.respond).toHaveBeenCalledWith("a", "Approve", "0f".repeat(32));
    c.onRequest(req("b", { digest_hex: "1e".repeat(32) }));
    await flush();
    await c.deny();
    expect(api.respond).toHaveBeenLastCalledWith("b", "Deny", "1e".repeat(32));
  });

  it("a refusal from core keeps the request on screen and shows its token", async () => {
    const { c, api, advance } = setup();
    api.respond.mockRejectedValueOnce("too_early");
    c.onRequest(req("a"));
    await flush();
    advance(GUARD_MS);
    await c.approve();
    expect(c.current()?.id).toBe("a");
    expect(c.getState().error).toBe("too_early");
    expect(c.getState().busy).toBe(false);
  });

  it("an unknown id from core drops the stale request", async () => {
    const { c, api, advance } = setup();
    api.respond.mockRejectedValueOnce("unknown_id");
    c.onRequest(req("a"));
    await flush();
    advance(GUARD_MS);
    await c.approve();
    expect(c.current()).toBeNull();
  });
});

describe("closing", () => {
  it("a Killed close empties the view", async () => {
    const { c } = setup();
    c.onRequest(req("a"));
    c.onRequest(req("b"));
    await flush();
    c.onClosed({ id: "a", outcome: "Killed" });
    c.onClosed({ id: "b", outcome: "Killed" });
    expect(c.getState().queue).toEqual([]);
    expect(c.getState().ended).toBe("killed");
    expect(c.approveEnabled()).toBe(false);
  });

  it("the Stop button clears the view at once and calls kill_agent", async () => {
    const { c, api } = setup();
    c.onRequest(req("a"));
    await flush();
    await c.kill();
    expect(api.kill).toHaveBeenCalledTimes(1);
    expect(c.current()).toBeNull();
  });

  it("the next request needs its own display ack and its own guard", async () => {
    const { c, api, advance } = setup();
    c.onRequest(req("a"));
    c.onRequest(req("b"));
    await flush();
    advance(GUARD_MS);
    c.onClosed({ id: "a", outcome: "Denied" });
    expect(c.current()?.id).toBe("b");
    expect(c.approveEnabled()).toBe(false);
    await flush();
    expect(api.displayed).toHaveBeenLastCalledWith("b");
    expect(c.approveEnabled()).toBe(false);
    advance(GUARD_MS);
    expect(c.approveEnabled()).toBe(true);
  });

  it("duplicate requests and unknown closes are ignored", async () => {
    const { c } = setup();
    c.onRequest(req("a"));
    c.onRequest(req("a"));
    expect(c.getState().queue).toHaveLength(1);
    c.onClosed({ id: "zzz", outcome: "Killed" });
    expect(c.getState().queue).toHaveLength(1);
  });
});
