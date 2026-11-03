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

## H-001 2026-11-03 T0 → reviewer-ARCH
Asked: Propose Step 1 architecture ADR.
Result: decision proposed; user approved with amendments (AX enable-first + spoken block notice; Windows foreground-window fallback; add `frame`; local-only default LLM).
Key points: sealed RedactedFrame in firewall; async traits; Tauri commands+events; fail-closed table.
Next: T1 → coder.

