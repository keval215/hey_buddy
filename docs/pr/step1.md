# Draft PR — Step 1: Foreground buddy, privacy-first

Head: `claude/trusting-pascal-ie39fj` → base: `main` · **DRAFT. Do not merge until the Windows exit test passes.**

## What
Hold a hotkey, speak, hear a local voice answer about what is on screen, with secrets removed before anything reaches a model. Windows first; macOS code is frozen (D-005).

- Crates: `frame`, `voice` (Whisper/Silero/Phonon/cpal, registry), `capture` (WGC + deny-list), `firewall` (sealed `RedactedFrame`/`RedactedScreenText`, secure-first walker, fail-closed), `llm` (exact-bytes `PreparedRequest`, digest-bound `CloudOptIn`, Ollama/Anthropic), `core` (turn engine, consent gate, settings, point parser), `apps/desktop` (tray, hotkey, overlay, settings, preview panel).
- Local-first: Ollama on loopback by default; cloud only via per-request preview + Send.
- ADRs: 0001 (architecture), 0002 (LLM, preview, text-first context). Decisions D-001…D-005 in `docs/agents/DECISIONS.md`.

## Verified here
fmt, clippy `-D warnings`, 350+ tests on fakes (incl. 29 e2e), `cargo deny`, dependency-direction checks.

## NOT verified (needs Windows) — see `docs/agents/VERIFY-LOCALLY.md`
Real Whisper/Silero/Phonon/mic/speaker, UIA/WGC adapters, tray/overlay/hotkey, DPI manifest, keychain, real Ollama/Anthropic calls, first CI run on GitHub.

## Test order
`docs/manual-tests.md` → "Step 1 EXIT TEST".
