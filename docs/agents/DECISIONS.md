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
