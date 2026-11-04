# Draft PR — Step 2: harden Step 1, then make the buddy act (safely)

Heads: `step2a/hardening` (2A) and `step2b/agent-harness` (2B, based on 2A) → base `main` · **DRAFT. Never merge before the user reviews the provisional ADRs 0003/0004 and runs the manual tests.**

## 2A hardening (done)
S2-2 engine hot-swap (EngineSlot) · S2-3 Kokoro text frontend (own CMUdict G2P, no espeak) · S2-4 click-only verified model downloader + Models screen · S2-5 title/URL leak hardening (scrubber, chrome drops, leakguard, audit doc) · S2-7 settings validation + deny-list editor.
BLOCKED-ON-USER: S2-1 (Windows findings), S2-6 (bench numbers).

## 2B agent phase (in progress; see docs/agents/STATE.md for the live board)
Done: S2-14a firewall `observe`/`revalidate` (sealed Observation/LiveTarget) · S2-14b `action` crate (validated actions, canonical bytes) · S2-11a/b `policy` crate (plan/v1 parser, 27-row tier table, PolicyGate, sealed ApprovedAction, single-use digest-bound approvals, observe mode ON by default, unattended Read/Reversible only) · S2-12 append-only audit log · S2-13 kill switch.
Not yet built (see STATE.md): driver crate + Windows adapter, engine/protocol, approval UI, injection defences, tasks, red-team suite, e2e wiring.

## Decisions
ADR 0003 and ADR 0004 are PROVISIONAL (D-006/D-007): the user was absent; autonomous-mode defaults per D-008. Review them first.

## Verified here
fmt, clippy -D warnings, 760+ tests on Linux, cargo deny, dependency-direction and tripwire greps. Reviewer-run mutation testing on the gate.

## NOT verified (needs Windows) — docs/agents/VERIFY-LOCALLY.md, docs/manual-tests.md
Everything cfg(windows): UIA readers, WGC, hotkeys (push-to-talk + kill), tray, overlay, keychain, downloads on real HTTPS, real models.
