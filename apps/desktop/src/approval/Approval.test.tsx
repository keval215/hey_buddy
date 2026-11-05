import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { ApprovalRequestDto } from "../ipc/ApprovalRequestDto";
import { ApprovalView } from "./Approval";
import type { ViewState } from "./controller";
import source from "./Approval.tsx?raw";

const plan: ApprovalRequestDto = {
  id: "0123456789abcdef0123456789abcdef",
  kind: "Plan",
  digest_hex: "cafebabe" + "0".repeat(56),
  summary: "Plan cafebabe: 3 steps (1 read, 1 reversible, 1 consequential, 0 refused)",
  lines: [
    { index: 0, op: "observe", target: null, text: null, tier: "Read", refused: false, ask_again: false },
    { index: 1, op: "scroll", target: null, text: null, tier: "Reversible", refused: false, ask_again: false },
    {
      index: 2,
      op: "click",
      target: "<img src=x onerror=alert(1)>Save\u{202E}\n",
      text: "<script>steal()</script>",
      tier: "Consequential",
      refused: false,
      ask_again: true,
    },
    { index: 3, op: "type", target: "Password", text: null, tier: "Consequential", refused: true, ask_again: false },
  ],
  live: false,
  changed: false,
  timeout_secs: 600,
};

function render(over: Partial<ViewState> = {}, now = 0, dto: ApprovalRequestDto = plan) {
  const state: ViewState = {
    queue: [dto],
    shownAt: 0,
    busy: false,
    error: null,
    ended: null,
    ...over,
  };
  return renderToStaticMarkup(
    <ApprovalView state={state} now={now} onApprove={vi.fn()} onDeny={vi.fn()} onKill={vi.fn()} />,
  );
}

describe("approval view", () => {
  it("has no autofocus anywhere and Approve starts disabled with a countdown", () => {
    const html = render({}, 100);
    expect(html.toLowerCase()).not.toContain("autofocus");
    expect(html).not.toContain("tabindex");
    const approve = /<button[^>]*disabled=""[^>]*>Approve \(0\.4 s\)<\/button>/.exec(html);
    expect(approve).not.toBeNull();
  });

  it("Approve is waiting while the window is not acknowledged", () => {
    const html = render({ shownAt: null });
    expect(html).toMatch(/disabled=""[^>]*>Approve \(waiting for the window\)/);
  });

  it("Approve unlocks after 500 ms; Deny and Stop are always enabled", () => {
    const early = render({}, 0);
    const late = render({}, 500);
    expect(late).toMatch(/<button type="button" class="[^"]*">Approve<\/button>/);
    expect(late).not.toMatch(/disabled=""[^>]*>Approve/);
    for (const html of [early, late]) {
      expect(html).toMatch(/<button type="button" class="[^"]*">Deny \(Esc\)<\/button>/);
      expect(html).toMatch(/<button type="button" class="[^"]*">Stop agent<\/button>/);
    }
  });

  it("renders hostile labels as inert text", () => {
    const html = render({}, 500);
    expect(html).not.toContain("<script");
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img src=x onerror=alert(1)&gt;Save");
    expect(html).toContain("&lt;script&gt;steal()&lt;/script&gt;");
    expect(html).not.toContain("\u{202E}");
    expect(html).not.toContain("\n");
  });

  it("shows ordered steps, tier badges, refused, will-ask-again and the digest echo", () => {
    const html = render({}, 500);
    const order = ["observe", "scroll", "click", "type"].map((op) => html.indexOf(`>${op}<`));
    expect(order.every((i) => i >= 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    for (const t of ["Read", "Reversible", "Consequential"]) expect(html).toContain(`>${t}<`);
    expect(html).toContain("refused, will not run");
    expect(html).toContain("will ask again");
    expect(html).toContain("digest cafebabe");
    expect(html).toContain("DRY RUN");
  });

  it("a step approval shows live, changed and its own Submit title", () => {
    const step: ApprovalRequestDto = {
      ...plan,
      kind: "Submit",
      live: true,
      changed: true,
      lines: [plan.lines[2]],
    };
    const html = render({}, 500, step);
    expect(html).toContain("LIVE");
    expect(html).toContain("changed since approved");
    expect(html).toContain("Approve SUBMIT");
  });

  it("an empty queue says so, and the Stop button stays; killed says stopped", () => {
    const idle = render({ queue: [] });
    expect(idle).toContain("Nothing is waiting");
    expect(idle).toContain("Stop agent");
    expect(idle).not.toContain("Deny");
    expect(render({ queue: [], ended: "killed" })).toContain("Stopped.");
  });

  it("an error token is shown as an alert", () => {
    expect(render({ error: "digest_mismatch" })).toContain('role="alert"');
  });

  it("the source never uses raw HTML injection or autofocus", () => {
    expect(source).not.toMatch(/dangerouslySetInnerHTML|innerHTML|autoFocus/);
  });
});
