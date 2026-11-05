# Draft PR — Step 2: harden Step 1, then make the buddy act (safely)

Heads: `step2a/hardening` (2A) and `step2b/agent-harness` (2B, based on 2A) → base `main` · **DRAFT. Never merge before the user reviews the provisional ADRs 0003/0004 and runs the manual tests.**

## 2A hardening (done)
S2-2 engine hot-swap (EngineSlot) · S2-3 Kokoro text frontend (own CMUdict G2P, no espeak) · S2-4 click-only verified model downloader + Models screen · S2-5 title/URL leak hardening (scrubber, chrome drops, leakguard, audit doc) · S2-7 settings validation + deny-list editor.
BLOCKED-ON-USER: S2-1 (Windows findings), S2-6 (bench numbers).

## 2B agent phase (done on Linux; Windows runtime unverified)
S2-14a firewall `observe`/`revalidate` · S2-14b `action` crate · S2-11 `policy` crate (plan/v1, tier table, PolicyGate, sealed ApprovedAction, digest-bound single-use approvals, observe mode ON) · S2-12 audit log · S2-13 kill switch · S2-14c/d `driver` crate + Windows actuator (cfg) · S2-10/15 `agent` crate (stdio protocol, MinimalEngine, sandboxed child, AppContainer+Job on Windows) + CoreModelGateway · S2-16 approval broker + capture-excluded window · S2-17/23 AgentSupervisor + runtime glue + Tauri commands + settings (agent_enabled/agent_mode default OFF) · S2-22 red-team suite (88 tests, 18+ mutations) and closed gaps (purchase words, confusable fold, drift `changed`).
Skipped/BLOCKED-ON-USER: S2-1, S2-6. Step 2 exit test: docs/manual-tests.md "Step 2 exit test (Windows)".

## Decisions
ADR 0003 and ADR 0004 are PROVISIONAL (D-006/D-007), plus D-009 (BindFacts): the user was absent; autonomous-mode defaults per D-008. Review them first.

## Verified here
fmt, clippy -D warnings, 760+ tests on Linux, cargo deny, dependency-direction and tripwire greps. Reviewer-run mutation testing on the gate.

## NOT verified (needs Windows) — docs/agents/VERIFY-LOCALLY.md, docs/manual-tests.md
Everything cfg(windows): UIA readers, WGC, hotkeys (push-to-talk + kill), tray, overlay, keychain, downloads on real HTTPS, real models.
