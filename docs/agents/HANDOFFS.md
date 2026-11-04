# Handoffs

_Owner: orch. Newest first. Max 60 entries; archive older ones to
`docs/agents/archive/`._

Format:

```
## H-S3-1 2026-11-04 S3-1 -> coder x2, reviewer
Asked: unsigned NSIS per-user installer + installer-check + icons.
Result: round 1 CHANGES REQUESTED (MAJOR icacls scope), round 2 accepted by orch (13 installer-check tests). NSIS build/CI artifact UNTESTED.

## H-S2-23b 2026-11-04 S2-23b -> coder, reviewer
Asked: runtime glue, Tauri commands, settings, exit-test doc, supervisor minors.
Result: APPROVED round 1 (4 MINOR deferred to S2-22).

## H-S2-17 2026-11-04 S2-17+23a -> coder, reviewer
Asked: agent supervisor wiring.
Result: APPROVED (5 MINOR); ReverifiedAtExecute judged SAFE-IF -> D-009.

## H-S2-16 2026-11-04 S2-16 -> coder x2, reviewer
Asked: approval broker + window + UI.
Result: round 1 CHANGES REQUESTED (MAJOR held-key repeat), round 2 accepted by orch.
Key points:
- 6 mutations caught by tests; kill-test waits now bounded.

## H-S2-10 2026-11-04 S2-10+15 -> coder x2, reviewer
Asked: agent crate, sandbox, gateway, llm schema.
Result: round 1 CHANGES REQUESTED (1 MAJOR Debug leak + minors), round 2 fixes accepted by orch (each fix has a test).
Key points:
- No path from engine to gate/driver; 29/36 mutations caught in review, gaps now tested.
- WindowsSandbox compile-checked only (untested on Windows).

## H-000 <YYYY-MM-DD HH:MM> T<n> → <coder|reviewer|reviewer-ARCH>
Asked: <one line>
Result: <DONE|BLOCKED|APPROVED|CHANGES REQUESTED|decision proposed>
Key points: <2–4 bullets>
Next: <what orch does next>
```

## H-018 2026-11-04 S2-2 -> coder, reviewer
Result: APPROVED round 1 (EngineSlot, request_engine_swap, 29 e2e tests). NOTE: most S2-2 code was swept into the S2-8/S2-9 ADR commits by orch `git add -A` mid-run (history mislabelled, code intact). Lesson: add specific paths while a coder runs.
Next: S2-3.

## H-021 2026-11-04 S2-4 -> coder (x2), reviewer (x2)
Result: APPROVED round 2. Resumable HTTPS-only sha256-verified downloader (core/download), Models table in settings, loopback fake-server tests. Round 1 MAJOR: 'unverified' marker was written after rename (fail-open) -> now before, with rollback.
Next: S2-5.

## H-020 2026-11-04 S2-3 -> coder (x2), reviewer (x2)
Result: APPROVED round 2. Own CMUdict parser + rule G2P (no espeak, no new crates); round 1 BLOCKER: Unicode lowercase length-change panic on 'İİİİam' fixed with ASCII lowercasing + table-driven adversarial fuzz.
Next: S2-4.

## H-019 2026-11-04 S2-8/S2-9 -> reviewer-ARCH (Opus) x2
Result: ADR 0003 and ADR 0004 drafted (PROPOSED), waiting on user approval + tier table sign-off.

## H-017 2026-11-03 T12-hardening -> coder, reviewer
Result: APPROVED round 1 (60 s recording cap finishes like a release; non-zero-origin Windows fallback test; log-capture sanity asserts; remaining-time preview; api-key cache primed).
Next: Step 1 complete -> final memory + PR go-ahead from user.

## H-016 2026-11-03 T12 -> coder, reviewer
Result: APPROVED round 1 (20 e2e tests on fakes; image only from fresh Verdict::Clear; consent only via prepare_and_consent; real-voice feature off by default).

## H-015 2026-11-03 T11 -> coder, reviewer
Result: APPROVED round 1 (core/consent.rs only minter of CloudOptIn; preview:// from same Arc; capability scoping, not id secrecy, is the control).

## H-014 2026-11-03 T10 -> coder, reviewer
Result: APPROVED round 1 (core::point both forms, core::api ts-rs, SettingsStore, listen-only overlay capability, PerMonitorV2 manifest).

## H-013 2026-11-03 T13 -> coder (x2), reviewer (x2)
Result: APPROVED round 2 (round 1 MAJOR: Windows TitleBar leaked window title; Tab labels and depth<=2 Group/Pane/Other labels never read).

## H-012 2026-11-03 T9.2 -> coder, reviewer
Result: APPROVED round 1 (PreparedRequest exact bytes; CloudOptIn gate; loopback-only Ollama; ring-based rustls; deny bans; CI grep).

## H-011 2026-11-03 T9.1-hardening -> coder, reviewer
Result: APPROVED round 1 (geometric drop over secure rects, Cf/tag-char stripping, intersects cross-check, multiple-roots block).

## H-010 2026-11-03 T9.1 → coder, reviewer
Asked: Firewall text API (check_text, RedactedScreenText, FakeTextSource), core::route, manual-tests.md.
Result: APPROVED round 1 with 6 MINOR hardening items (geometric containment around secure rects, strip tag chars/variation selectors, cap/sanitise app id, intersects in cross-check, reject 2nd depth-0 root, meaningful_nodes semantics).
Next: hardening pass, then T9.2.

## H-009 2026-11-03 T8 → coder (x2), reviewer (x2)
Asked: Firewall v0 (sealed RedactedFrame, blackout, fail-closed PrivacyFirewall, mac/Win detectors, fakes).
Result: APPROVED round 2.
Key points: round 1 found 4 MAJOR (compile_fail wrong reason, Windows DPI/display hint, limitations undocumented, NaN size skipped); fixed. SecureFieldDetector::secure_fields(&self) now display-independent (ADR line amended). Windows uses physical px matching capture; needs PerMonitorV2 manifest.
Next: T9 [ARCH] → reviewer Opus.

## H-008 2026-11-03 T7 → coder (x2), reviewer (x2)
Asked: frame + capture crates, deny-list, mac/Win impls, fake.
Result: code APPROVED round 2; ticket awaits user sign-off on banking defaults.
Key points: round 1: empty-list-via-config hole, foreground re-check, docs; policy fns shared by fake and real; errors fixed strings, WindowInfo has no title.
Next: T8 → coder.

## H-007 2026-11-03 T6 → coder, reviewer
Asked: Latency bench with fakes + optional real stages.
Result: APPROVED round 1.
Key points: tools/bench lib+CLI; stages real only with features + models dir; JSON has no content; `meaningful` flag gates pass/fail.
Next: T7 → coder.

## H-006 2026-11-03 T5 → coder, reviewer
Asked: Phonon TTS (ptts), Kokoro fallback, cpal streaming playback.
Result: APPROVED round 1 (Kokoro partial, accepted as honest stub).
Key points: ptts 0.6 from crates.io, default-features off (no network); StreamingTts core; Registry::model_dir; workspace MSRV -> 1.85.
Next: T6 → coder.

## H-005 2026-11-03 T4 → coder (x3 runs), reviewer
Asked: Whisper STT, Silero VAD, cpal mic, behind features.
Result: APPROVED round 1 (after licence hard stop resolved by user: D-002 Unlicense).
Key points: Silero tensor names read from ONNX at load, mapped by dtype/rank; ids versioned (`silero-vad-v5`); whisper batch-only; model tests skip when absent.
Next: T5 → coder.

## H-004 2026-11-03 T3 → coder, reviewer
Asked: Voice traits, fakes, resampler, e2e test.
Result: APPROVED round 1.
Key points: traits per ADR; fakes behind `fakes` (tokio optional, not in desktop tree); Debug impls print lengths only.
Next: T4 → coder.

## H-003 2026-11-03 T2 → coder, reviewer
Asked: Model registry + loader.
Result: APPROVED round 1.
Key points: loader in `voice::registry`; rejects non-commercial defaults, bundled=true, bad sha256; all URLs/sha TODO; minor follow-ups in STATE.
Next: T3 → coder.

## H-002 2026-11-03 T1 → coder, reviewer (2 rounds)
Asked: Repo scaffold and CI.
Result: APPROVED after round 2.
Key points: crate `core` renamed package `buddy-core` (name clash with Rust core); round 1 found CI missing frontend build + Windows icon.ico; deny.toml adds Apache-2.0 WITH LLVM-exception and Unicode-3.0 (permissive, noted).
Next: T2 → coder.

## H-001 2026-11-03 T0 → reviewer-ARCH
Asked: Propose Step 1 architecture ADR.
Result: decision proposed; user approved with amendments (AX enable-first + spoken block notice; Windows foreground-window fallback; add `frame`; local-only default LLM).
Key points: sealed RedactedFrame in firewall; async traits; Tauri commands+events; fail-closed table.
Next: T1 → coder.

