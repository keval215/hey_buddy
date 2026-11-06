import { describe, expect, it } from "vitest";
import type { UpdateStatusDto } from "../ipc/UpdateStatusDto";
import {
  NOT_CONFIGURED_TEXT,
  advisories,
  canCheck,
  canDownload,
  canInstall,
  emptyUpdateView,
  installArgs,
  isDowngrade,
  onUpdateDone,
  onUpdateFailed,
  onUpdateProgress,
  percent,
  resultText,
  updateErrorText,
} from "./logic";

const base: UpdateStatusDto = {
  configured: true,
  current_version: "0.1.0",
  result: "not_checked",
  version: null,
  size: null,
  notes: null,
  current_revoked: false,
  below_min_supported: false,
  downloaded: false,
  busy: false,
  state_issue: false,
};
const available: UpdateStatusDto = {
  ...base,
  result: "available",
  version: "0.2.0",
  size: 7340032,
  notes: "Fixes things.",
};
const downgrade: UpdateStatusDto = {
  ...base,
  current_version: "0.1.1",
  result: "downgrade_only",
  version: "0.1.0",
  current_revoked: true,
};

describe("button availability", () => {
  it("never offers a check in a build without keys", () => {
    const s = { ...base, configured: false, result: "updates_not_configured" as const };
    expect(canCheck(s, false)).toBe(false);
    expect(canDownload(s, emptyUpdateView, false)).toBe(false);
    expect(canInstall(s, emptyUpdateView, false)).toBe(false);
    expect(resultText(s)).toBe(NOT_CONFIGURED_TEXT);
  });

  it("check needs a configured build and nothing running", () => {
    expect(canCheck(null, false)).toBe(false);
    expect(canCheck(base, false)).toBe(true);
    expect(canCheck(base, true)).toBe(false);
    expect(canCheck({ ...base, busy: true }, false)).toBe(false);
  });

  it("download needs an offer and is separate from install", () => {
    expect(canDownload(base, emptyUpdateView, false)).toBe(false);
    expect(canDownload(available, emptyUpdateView, false)).toBe(true);
    expect(canDownload({ ...available, downloaded: true }, emptyUpdateView, false)).toBe(false);
    expect(canDownload({ ...base, result: "up_to_date" }, emptyUpdateView, false)).toBe(false);
    const running = onUpdateProgress(emptyUpdateView, { id: "update", bytes: 1, total: 10 });
    expect(canDownload(available, running, false)).toBe(false);
  });

  it("install needs the verified download and no running step", () => {
    expect(canInstall(available, emptyUpdateView, false)).toBe(false);
    const ready = { ...available, downloaded: true };
    expect(canInstall(ready, emptyUpdateView, false)).toBe(true);
    expect(canInstall(ready, emptyUpdateView, true)).toBe(false);
    expect(canInstall({ ...ready, busy: true }, emptyUpdateView, false)).toBe(false);
    expect(canInstall({ ...base, downloaded: true }, emptyUpdateView, false)).toBe(false);
  });
});

describe("install arguments", () => {
  it("an upgrade sends the version the user saw and never the downgrade flag", () => {
    expect(installArgs(available, false)).toEqual({ version: "0.2.0", allowDowngrade: false });
    expect(installArgs(available, true)).toEqual({ version: "0.2.0", allowDowngrade: false });
  });

  it("a downgrade is only sent after the explicit confirm", () => {
    expect(isDowngrade(downgrade)).toBe(true);
    expect(installArgs(downgrade, false)).toBeNull();
    expect(installArgs(downgrade, true)).toEqual({ version: "0.1.0", allowDowngrade: true });
  });

  it("nothing is sent without an offer", () => {
    expect(installArgs(base, true)).toBeNull();
    expect(installArgs({ ...base, result: "up_to_date" }, true)).toBeNull();
  });
});

describe("texts", () => {
  it("describes each result in a fixed sentence", () => {
    expect(resultText({ ...base, result: "up_to_date" })).toContain("up to date");
    expect(resultText(available)).toContain("0.2.0");
    expect(resultText(available)).toContain("7.0 MB");
    expect(resultText(downgrade)).toContain("withdrawn");
    expect(resultText(base)).toContain("Not checked yet");
  });

  it("maps every token to a sentence and never echoes unknown text", () => {
    for (const t of [
      "updates_not_configured",
      "bad_signature",
      "no_trusted_signature",
      "manifest_expired",
      "replayed_manifest",
      "stale_manifest",
      "bad_redirect",
      "hash_mismatch",
      "installer_needs_admin",
      "launch_blocked",
      "downgrade_not_allowed",
      "revoked",
    ]) {
      expect(updateErrorText(t)).not.toBe(updateErrorText("zzz"));
      expect(updateErrorText(t)).not.toContain(t);
    }
    expect(updateErrorText("<script>alert(1)</script>")).toBe("The update step failed.");
    expect(updateErrorText(new Error("https://evil.example/x"))).toBe("The update step failed.");
    expect(updateErrorText("launch_blocked")).toContain("release page");
  });

  it("shows advisories from the verified flags", () => {
    expect(advisories(base)).toEqual([]);
    expect(advisories({ ...available, current_revoked: true }).join(" ")).toContain("withdrawn");
    expect(advisories({ ...available, below_min_supported: true }).join(" ")).toContain(
      "no longer supported",
    );
    expect(advisories({ ...base, state_issue: true }).join(" ")).toContain("reset");
    // The downgrade card already says it; no duplicate line.
    expect(advisories(downgrade)).toEqual([]);
  });
});

describe("download events", () => {
  it("tracks progress, completion and failures of the update id only", () => {
    let v = onUpdateProgress(emptyUpdateView, { id: "update", bytes: 50, total: 200 });
    expect(percent(v)).toBe(25);
    expect(onUpdateProgress(v, { id: "whisper-base", bytes: 1, total: 2 })).toBe(v);
    v = onUpdateDone(v, { id: "update" });
    expect(v).toEqual({ progress: null, error: null, finished: true });
    v = onUpdateFailed(v, { id: "update", code: "checksum_mismatch" });
    expect(v.error).toBe("checksum_mismatch");
    expect(onUpdateFailed(v, { id: "update", code: "cancelled" }).error).toBeNull();
    expect(onUpdateFailed(v, { id: "other", code: "x" })).toBe(v);
  });

  it("has no percentage without a total", () => {
    expect(percent(onUpdateProgress(emptyUpdateView, { id: "update", bytes: 5, total: null }))).toBeNull();
    expect(percent(emptyUpdateView)).toBeNull();
  });
});

// ---- no automatic check anywhere (source scan) ----------------------------------------------

const raw = import.meta.glob("../**/*.{ts,tsx}", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;
// Files in this folder come back as "./x"; normalise to "../updates/x".
const sources: Record<string, string> = Object.fromEntries(
  Object.entries(raw).map(([k, v]) => [k.startsWith("./") ? `../updates/${k.slice(2)}` : k, v]),
);

describe("no automatic update calls", () => {
  const callers = (name: string) =>
    Object.entries(sources)
      .filter(([f]) => !f.endsWith(".test.ts") && !f.includes("/ipc/"))
      .filter(([, src]) => src.includes(`${name}(`))
      .map(([f]) => f);

  it("updateCheck / updateDownload / updateInstall are only called from the panel's click handlers", () => {
    for (const name of [".updateCheck", ".updateDownload", ".updateInstall"]) {
      expect(callers(name)).toEqual(["../updates/UpdatePanel.tsx"]);
    }
    expect(Object.keys(sources).length).toBeGreaterThan(5); // the scan is alive
  });

  it("the panel calls them inside handler functions, never in an effect, timer or at render", () => {
    const panel = sources["../updates/UpdatePanel.tsx"] ?? "";
    expect(panel).toContain("const check = () => {");
    expect(panel).toContain("onClick={check}");
    expect(panel).toContain("onClick={download}");
    expect(panel).toContain("onClick={install}");
    // The effects only read local status and listen to events.
    const effects = panel.split("useEffect(").slice(1);
    expect(effects.length).toBe(2);
    for (const e of effects) {
      expect(e.slice(0, e.indexOf("[refresh]"))).not.toMatch(
        /updateCheck|updateDownload|updateInstall/,
      );
    }
    expect(panel).not.toMatch(/setInterval|setTimeout/);
  });

  it("the settings page and client do not call the update commands on their own", () => {
    const app = sources["../App.tsx"] ?? "";
    expect(app).not.toMatch(/updateCheck|updateDownload|updateInstall/);
    expect(app).toContain("<UpdatePanel />");
  });
});
