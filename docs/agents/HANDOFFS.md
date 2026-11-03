# Handoffs

_Owner: orch. Newest first. Max 60 entries; archive older ones to
`docs/agents/archive/`._

Format:

```
## H-000 <YYYY-MM-DD HH:MM> T<n> → <coder|reviewer|reviewer-ARCH>
Asked: <one line>
Result: <DONE|BLOCKED|APPROVED|CHANGES REQUESTED|decision proposed>
Key points: <2–4 bullets>
Next: <what orch does next>
```

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

