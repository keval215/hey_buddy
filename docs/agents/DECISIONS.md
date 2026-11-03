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
