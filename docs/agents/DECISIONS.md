# Decisions

_Owner: orch. One entry per approved architecture decision, newest first. Full
records live in `docs/adr/`._

Format:

```
## D-000 <title> — <YYYY-MM-DD>
Decision: <1–2 lines>
Why: <1–2 lines>
ADR: docs/adr/NNNN-<slug>.md
Status: accepted | superseded by D-xxx
```

## D-000 Model policy — 2026-11-03
Decision: All agents run on Sonnet 5.5; Opus 5.5 is used only for architecture decisions via reviewer ARCHITECTURE MODE.
Why: Keep Claude Pro usage within limits; spend the strongest model only where mistakes are expensive.
ADR: none (team policy)
Status: accepted

## D-001 Step 1 architecture — 2026-11-03
Decision: Seven crates incl. new pure-data `frame`; `RedactedFrame` sealed in `firewall`, `llm` takes only it (compile-time). Async traits (tokio, async-trait, BoxStream). Only `core` wires crates; app depends only on `core`. Fail closed; blocked/paused turns continue voice-only and tell the user aloud why. Unreadable AX tree: try enabling accessibility first, then block. Windows deny-listed app visible: refuse full-display capture, fall back to foreground-window capture if not deny-listed. LLM local-only by default; cloud needs per-request opt-in (T9).
Why: Real compile-time privacy guarantee; swappable, testable-on-Linux parts.
ADR: docs/adr/0001-step1-architecture.md
Status: accepted (user approved)
Note: crate `core` has package name `buddy-core` (T1, name clash with Rust `core`).

## D-002 Licence allowlist extended (note, not ARCH) — 2026-11-03
Decision: Allowed dependency licences are now MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unlicense, CC0-1.0, 0BSD. Unlicense exception approved by user for whisper-rs / whisper-rs-sys (T4). `Apache-2.0 WITH LLVM-exception` and `Unicode-3.0` remain allowed in deny.toml (T1 note).
Why: whisper-rs is the mature whisper.cpp binding; Unlicense is public-domain-like and Apache-2.0 compatible.
ADR: none
Status: accepted

## D-004 Licence policy relaxed + RTX 2050 (4 GB) default hardware profile (note, not ARCH) — 2026-11-03
Decision: Code deps: any permissive licence auto-approved; only GPL/AGPL/LGPL and non-commercial/source-available code need user approval. Models: no licence restrictions; never committed; optional user download with licence shown in the download UI; `commercial_ok` is informational only (loader gate removed). `cargo deny` stays in CI with the expanded allowlist (supersedes D-002 list). Weights are never committed.
Target dev machine RTX 2050 / 4 GB VRAM: STT Whisper small on CPU (base fallback; never GPU), TTS Phonon 31m CPU, text LLM Ollama Qwen3 4B Q4 (Qwen3 1.7B low-memory), vision Qwen3-VL 2B via Ollama (see STATE: routing question open — must never receive a frame that did not pass the firewall).
Why: Reduce friction; match real hardware.
ADR: none
Status: accepted (user)

## D-003 T9: LLM provider, exact-bytes preview, text-first screen context — 2026-11-03
Decision: PreparedRequest (exact body bytes) is the single object for preview/digest/CloudOptIn/transport; cloud calls need a digest-bound one-use CloudOptIn minted only in core/consent.rs from preview Send (no always-allow). Local-only default (Ollama loopback). Single-shot turns. Default screen context = firewall-redacted accessibility-tree text (RedactedScreenText, sealed in firewall; `Firewall::check_text` additive, fail-closed default); VLM only for firewall-Clear frames via pure `core::route`. reqwest+rustls(ring); keyring macOS/Windows only. Amends ADR 0001 (check_text, AppDenyPolicy, LlmRequest/ScreenContext, IPC).
Why: previewed bytes == sent bytes; redaction in our code before any model; unreadable tree never yields an image.
ADR: docs/adr/0002-llm-provider-and-preview.md
Status: accepted (user ack of Addendum A + defaults 2026-11-03)

## D-005 Windows-only for now (note, not ARCH) — 2026-11-04
Decision: Windows is the only supported/CI-gated desktop target. The macOS CI job is removed. Existing macOS code (capture/macos.rs, firewall/platform/macos*.rs, desktop glue) stays in the tree behind cfg(target_os = "macos"), is not CI-gated, not supported, and not to be extended until macOS returns. Linux CI remains for fmt/clippy/tests (fakes) only.
Why: user's dev machine is Windows; macOS code could not be compiled/verified here and failed first CI.
ADR: none
Status: accepted (user)

## D-006 ADR 0003 agent engine, tiers, approvals — 2026-11-04 — PROVISIONAL (pending user review)
Decision: as docs/adr/0003-agent-engine-and-approvals.md (in-house plan-then-execute engine, sandboxed child, sealed ApprovedAction, digest-bound single-use approvals, kill switch). Open questions 1-5 answered with the recommended answers (yes). Autonomous-mode override (user, 2026-11-04): tier defaults Read=allow; Reversible=dry-run first then allow; Consequential=approval every time; unknown=Consequential; Never=deny; no standing grants; observe mode ON by default; unattended runs Read+Reversible only. Where ADR 0003 is stricter, ADR 0003 wins (tightening is always allowed).
Status: provisional — pending user review

## D-007 ADR 0004 desktop driver and observation — 2026-11-04 — PROVISIONAL (pending user review)
Decision: as docs/adr/0004-desktop-driver.md (in-house Windows UIA driver; firewall owns observe/revalidate; one observation = one action; semantic clicks only; SendInput typing in <=16-char focus-guarded chunks; built-in restricted apps). Open questions 1-5 answered yes (recommended).
Status: provisional — pending user review

## D-008 Autonomous mode until end of Step 4 — 2026-11-04
Decision: user authorised autonomous execution of Step 1 finish, Step 2A+2B, Step 3 (packaging/installer/first-run wizard/auto-update design), Step 4 (firewall v1: OCR+PII, stronger lock/auth detection, threat model, internal adversarial review). Windows priority; macOS/Linux compile-check only. Hard stops that REMAIN: weakening the firewall; copyleft/non-commercial CODE deps; secrets/paid services (so NO code-signing certificates, no paid CI); force-push. Branches: step2a/hardening, step2b/agent-harness, step3/packaging, step4/firewall-v1; one DRAFT PR per step; never merge, never mark ready. Facts needing a real machine => BLOCKED-ON-USER + "User must verify locally", move on. Max 3 coder/review rounds then BLOCKED. Resume from STATE.md after a usage-limit stop. Stop condition: usage budget (orch cannot see credits; works until the limit stops the session; opens a draft PR first/at each step end so nothing is stranded).
Status: accepted (user)

## D-009 BindFacts for the agent supervisor — 2026-11-04 — PROVISIONAL (pending user review)
Decision: `UnknownFacts` (only reads run) is the default. `ReverifiedAtExecute` (bind-time not-elevated/not-secure claims, enforced by Firewall::revalidate + LiveTarget::verify_focus before every input, fail closed) may be chosen by the wiring only together with the real Windows actuator (S2-14d) and the manual Windows check. `AgentDriver` impls must keep both re-checks. Scheduled origin = Read-only; a plan with any Never step is refused whole (stricter than ADR).
Status: provisional — pending user review

## D-010 ADR 0005 auto-update — 2026-11-04 — PROVISIONAL (pending user review)
Decision: user-clicked check only (no polling) of one fixed GitHub Releases `latest/download/update-manifest.json` (HTTPS, host-allowlisted redirects). DSSE-style envelope, ed25519 via `ring` (already in tree) over PAE(payloadType, exact bytes); embedded keys only (2 offline planned), stateless rotation via endorsement. Pure crate `update` (no IO/network, CI tree guard); HTTP+download in buddy-core via generalised S2-4 downloader. Install = separate click: private-ACL dir copy, hash via deny-write handle held across CreateProcessW (no args). TRUSTED_KEYS empty => `updates_not_configured`, no network call. Downgrade only if current revoked + confirm. Open Qs for user: ADR §13. See docs/adr/0005-auto-update.md.
Status: provisional — pending user review
