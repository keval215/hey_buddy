# Step 4 — Firewall v1: local OCR, PII redaction, stronger lock/auth detection, threat model

**Goal:** the privacy firewall also protects what accessibility trees cannot see (text drawn in canvases, images, PDFs, games) and what looks like personal data, and gets a written threat model plus an internal adversarial review. Nothing in this step may weaken an existing fail-closed rule.

**Branch:** `step4/firewall-v1` (based on `step3/packaging`). **Platform:** Windows first (D-005).
**Autonomous-mode rules (D-008):** hard stops stay: weakening the firewall; copyleft/non-commercial CODE dependencies (models: any licence is allowed as an optional user download, shown in the UI); no secrets/paid services. Max 3 rounds; BLOCKED-ON-USER for real-machine facts. Outside review is the user's, later.

Ticket template: Why · Work · Tests · Acceptance · Verified by.

## S4-1 [ARCH] OCR engine and image redaction pipeline → ADR 0006
Why: pixels can contain text the accessibility tree does not (canvas, PDF viewers, images, remote desktops).
Work (Opus drafts `docs/adr/0006-ocr-and-image-redaction.md`; recorded provisional): (1) Options: Windows.Media.Ocr (OS API, no model to ship, language-pack dependent), Tesseract (Apache-2.0; native dependency/bindings), `ocrs`-style Rust engines (permissive; model download), ONNX PaddleOCR-class models (model download) — compare licence (code AND model), size, accuracy, latency on CPU, Windows availability, offline, fail-closed behaviour. Recommend one behind an `OcrEngine` trait with a fake. (2) Pipeline: capture → deny-list/secure-field blackout (existing) → OCR → PII detector (S4-2) → blackout of PII boxes (padded) → `RedactedFrame`; a frame never leaves the firewall unless OCR succeeded within its deadline; OCR error/timeout/oversize ⇒ `Blocked(OcrFailed)` for the IMAGE path (text path unaffected). (3) Where OCR may run (firewall crate vs separate crate with sealed output), type-level guarantee that only the firewall can mint `RedactedFrame` still holds. (4) Latency budget on the RTX 2050 laptop class (OCR on CPU, never GPU contention with the LLM), downscaling policy, caching rules (never write frames or OCR text to disk). (5) Threat table (adversarial images: tiny text, low contrast, rotated/curved text, QR codes, homoglyphs, OCR-evasion).
Acceptance: [ ] ADR with options/decision/consequences/threat table. Verified by: user reads later.

## S4-2 PII detector (pure, shared by text and image paths)
Why: one detector, tested hard.
Work: crate/module `pii` (no OS deps): detect emails, phone numbers (international), payment cards (Luhn, 13–19 digits with separators), IBAN (mod-97), US SSN, Indian Aadhaar (Verhoeff) and PAN, passport-like labelled numbers, IPv4/IPv6, API keys/tokens (known prefixes, high-entropy), JWTs, PEM private-key blocks, `password:`-style labelled values, URLs with credentials/tokens, crypto wallet addresses. Output spans with kind + confidence; deterministic; hand-written (no regex crate unless permissive and already in the tree). Language: English + digits/symbols; document limits. Multi-line merge for OCR line splits. A `PiiPolicy` (kinds to always redact; default: all).
Tests: ≥200 table-driven positives/negatives per kind (synthetic data only — never real personal data), unicode/bidi/zero-width evasion, spacing/ dash variants, adversarial near-misses (order numbers vs card numbers), no-panic fuzz with seeded generator, time-bounded on 1 MB input.
Acceptance: [ ] tables pass; [ ] no panics/quadratic blowups.
Verified by: CI.

## S4-3 OCR adapters and fakes
Work: `OcrEngine` trait per ADR 0006 (`recognize(&Frame view) -> Vec<OcrLine{text, rect, confidence}>`, deadline, size caps); `FakeOcr` (scripted lines/rects, errors, delay, panic); Windows adapter behind cfg (Windows.Media.Ocr via the `windows` crate, or the ADR's choice); macOS/Linux `UnsupportedOcr` ⇒ image path blocked. OCR input is the frame AFTER the existing blackouts (so OCR never sees secure-field pixels) — assert in a test.
Tests: fake-based; adapter compile-checks; deadline/panic/oversize ⇒ error tokens; no text in logs/errors.
Acceptance: [ ] image path blocks when OCR unavailable; [ ] OCR never sees blacked-out regions.
Verified by: CI; user on Windows (BLOCKED-ON-USER for the real adapter).

## S4-4 Firewall v1 image pipeline
Why: wire S4-2/S4-3 into `Firewall::check`.
Work: extend `PrivacyFirewall` with an optional OCR stage (`with_ocr(engine, policy)`); PII boxes padded and blacked out; `RedactedFrame.redactions` includes them; new `BlockReason::OcrFailed`/`OcrUnavailable`; `platform_firewall()` wires the Windows engine; settings: `image_pii_redaction` (default ON; turning OFF is NOT offered in v1 — the stage is mandatory for the image path); OCR runs off the audio/turn threads with a deadline; preflight/stability semantics unchanged. Update core routing text: image path now also needs OCR success; spoken reasons for OcrFailed. ALSO (from the S2-5 review, pixel path): black out in the image the rects of the chrome nodes the text redactor drops (address bar, tab strip, bookmarks bar, download shelf — the walker already knows their rects) and the window caption/title-bar band (top band of the window rect, height from the caption metrics or a conservative fixed fraction) BEFORE the frame leaves the firewall; this only adds redactions; tests with fixtures proving title/URL pixel bands are black.
Tests: fixture frames (synthetic card number/email/token drawn as pixels using a test font renderer) with `FakeOcr` returning their boxes ⇒ pixels black in the verdict frame; OCR error ⇒ Blocked; timeout ⇒ Blocked; PII at frame edges; rotated boxes expand to bounding box; overlapping boxes; huge box counts capped ⇒ Blocked rather than skipped; e2e vision path still passes and now includes the PII stage.
Acceptance: [ ] no PII pixel survives in fixtures; [ ] every OCR failure blocks the image path; [ ] text path unchanged.
Verified by: CI.

## S4-5 PII scrub on the text path
Work: apply `pii` to emitted `RedactedScreenText` lines (after S2-5 URL/token scrubbing): replace spans with `[email]`, `[card]`, `[phone]`, `[id]`, `[secret]` etc.; never changes `#id` numbering or POINT mapping; record `pii_hidden` count in coverage/preview. Tests: golden screen-text with PII fixtures; hostile near-misses; performance cap.
Acceptance: [ ] markers absent from request body/preview/logs. Verified by: CI.

## S4-6 Stronger lock/auth detection
Why: the existing probe reports only lock screen / secure desktop / secure input.
Work: add signals (each behind a trait with fakes, Windows adapters behind cfg): UAC consent surface and credential-provider windows (exe + window class lists), Windows Hello / sign-in UI, screensaver/monitor-off (power events), session switch/remote (WTS notifications), browser login pages (page has a password field ⇒ already `[hidden]`, plus a "login context" flag when labels match sign-in/2FA/OTP lexicons ⇒ image path paused), password-manager popups (deny-list apps + their window classes), clipboard-history overlays; hysteresis (stay Paused N ms after the signal clears); fail closed on probe errors; `PauseReason::{AuthPrompt, Uac, CredentialUi, LoginContext, ScreenSaver, RemoteSession}`.
Tests: fake probes: each signal pauses; flapping signals use hysteresis; probe error blocks; combined signals; spoken reasons table.
Acceptance: [ ] new reasons wired through core spoken messages; [ ] manual checks listed.
Verified by: CI; user on Windows (BLOCKED-ON-USER).

## S4-7 Threat-model document
Work: `docs/threat-model.md`: assets, adversaries, trust boundaries, data-flow diagram (text), STRIDE-style table per component (capture, firewall, llm/consent, download/update, agent engine, policy gate, driver, webview/IPC, installer), existing mitigations mapped to tests (file:test names), residual risks and explicit non-goals (canvas passwords invisible to both AX and OCR-misses, shoulder surfing, malware with user privileges, compromised OS), assumptions about Windows isolation, supply-chain section (deps, models, updates).
Acceptance: [ ] every row links to a test or is marked "gap". Verified by: user later.

## S4-8 Internal adversarial review pass
Why: find holes before the user's outside reviewer does.
Work: (1) A reviewer agent (Sonnet) acts as attacker with the threat model: reads firewall, consent, gate, driver, downloader, update verifier, IPC/capabilities, and writes `docs/adversarial-review-1.md` — findings with severity, repro as a failing test where possible. (2) Orch turns each finding into a fix ticket (hard stops respected: no fix may weaken the firewall); coder fixes with regression tests; reviewer re-checks. (3) Residual findings documented.
Acceptance: [ ] findings file; [ ] all BLOCKER/MAJOR fixed or marked BLOCKED with reason. Verified by: CI + user later.

## S4-9 Deterministic fuzz/property suite in CI
Work: seeded, deterministic fuzz tests (no nightly, no new heavy deps) for: sanitiser, URL/token scrubber, PII detector, plan parser, protocol line parser, manifest parser, registry loader, path validators; budgeted runtime (<60 s in CI).
Acceptance: [ ] suite runs in the Linux CI job. Verified by: CI.

## S4-10 Step 4 exit test and user checklist
Work: manual tests for Windows: image with a card number/email/API key drawn in a canvas page and a PDF ⇒ preview shows black boxes; OCR missing ⇒ image path blocked with spoken reason; login page/2FA/UAC/Windows Hello ⇒ paused; password manager popup ⇒ nothing; `STATE.md` updated.
Acceptance: [ ] checklist written. Verified by: user.

## Definition of done for Step 4
- [ ] All tickets approved or BLOCKED with reason
- [ ] CI green on Linux and Windows
- [ ] Threat model + adversarial review documents committed
- [ ] Draft PR open (never merged by orch)
