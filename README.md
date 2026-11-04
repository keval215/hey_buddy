# hey_buddy

An open-source, local-first desktop AI buddy. It sits next to your cursor, sees your screen
when you press a hotkey, talks with you by voice, and (later) runs background agents that
operate your apps safely.

**The desktop buddy that can't see your passwords, runs its voice on your own machine, and
asks before it acts.**

## Status

Early development (Step 1: voice + screen-aware buddy). The whole push-to-talk turn is wired
(hotkey, firewall, screen text, speech to text, local LLM, text to speech, overlay pointing) and
tested end to end on fakes; running it for real needs the `real-voice` build below and is still
being verified on Windows (macOS support is deferred). See `docs/STEP1.md` for the plan and `docs/adr/` for
architecture decisions.

## Principles

1. Privacy first: redaction happens in our own code before data reaches any model.
2. Fail closed: if capture can't be made safe, it stays off.
3. Local first: voice, redaction and logs run on your device; cloud is opt-in per request.
4. Swappable parts behind small traits.
5. Clean licensing (Apache-2.0 project; permissive dependencies only).

## Development

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm install && pnpm lint && pnpm typecheck && pnpm test
pnpm --filter desktop tauri dev
```

### Real voice engines

By default the app is built WITHOUT the voice engines (CI and Linux stay light): it starts with
a "Voice engines not built" status. To run the real thing (whisper.cpp STT, Silero VAD, Phonon
TTS, cpal microphone/speaker):

```
cd apps/desktop && pnpm tauri dev --features real-voice
# or: cd apps/desktop/src-tauri && cargo tauri dev --features real-voice
```

It needs cmake + clang at build time (and `libasound2-dev` on Linux), the model files in
`<app data folder>/models` (or `HEY_BUDDY_MODELS_DIR`; nothing is downloaded unless you click
Download on the Models table in Settings, which is HTTPS only, resumable and checksum-verified;
registry entries without a download URL yet say "no download source yet"; weights are never
committed),
`ORT_DYLIB_PATH` pointing at onnxruntime, and Ollama with `qwen3:4b` pulled. Step by step:
`docs/manual-tests.md` ("Step 1 EXIT TEST").

On Linux the desktop crate needs the Tauri system libraries (webkit2gtk 4.1, GTK 3,
libayatana-appindicator, librsvg). Without them, plain `cargo build`/`cargo test` skip the
desktop crate (it is not a default member); `--workspace` includes it.

## Latency benchmark

`tools/bench` measures speech end to first audio: synthetic audio, then VAD and segmenter, STT,
a fake LLM (scripted deltas with artificial delay), sentence chunking, TTS, and a timestamped
fake audio sink. It prints a table and writes `bench-results.json` (git-ignored). The JSON holds
timings, which stages were real or fake, and host os/arch. It never holds audio or text.

Hardware profile: `--profile <name>` (default `default`) reads a profile from
`models/registry.json` and reports it in the table and JSON. The `default` profile is the
RTX 2050 (4 GB VRAM) dev machine: STT is whisper-small on CPU, TTS is phonon-31m on CPU, and the
LLM (qwen3-4b-q4) and VLM (qwen3-vl-2b) run on the GPU through Ollama. STT must never use the
GPU: the registry loader rejects a profile that assigns it there, so the GPU stays free for the
LLM. `low-memory` uses whisper-base and qwen3-1.7b with vision disabled.

Targets: the `default` profile uses `rtx2050-4gb`, 2.0 s speech end to first audio. This is
provisional (`provisional: true` in the JSON and the table) until measured on that machine. Other
profiles use the host target: under 1.0 s on Apple silicon, under 1.5 s on CPU laptops. The
pass/fail flag is only meaningful (and labelled so) when both STT and TTS are real. The LLM is
always a fake, so the number excludes real model time. Ollama model swapping and cold load are
excluded from the steady-state number; the LLM is not run, so `llm_cold_load_ms` is `null`.
Segmenter hangover time is not counted: the clock starts when speech end is detected.

Without models (fakes only; works anywhere, no cmake or audio device needed):

```
cargo run --release -p bench
cargo run --release -p bench -- --out my-results.json --stt-delay-ms 300 --tts-delay-ms 200
```

With models on your own Mac or Windows machine. Put the files in a models dir:
`<dir>/silero-vad-v5.onnx`, `<dir>/whisper-small.bin` (or another id via `--stt-model`),
and the `<dir>/phonon-31m/` checkpoint directory. Then:

```
export HEY_BUDDY_MODELS_DIR="$HOME/hey_buddy_models"      # Windows PowerShell: $env:HEY_BUDDY_MODELS_DIR="..."
export ORT_DYLIB_PATH=/path/to/libonnxruntime.dylib        # only for vad-silero
cargo run --release -p bench --features stt-whisper,vad-silero,tts-phonon
```

Each stage prints `using real X` or `using fake X (reason)`. A stage falls back to a fake only
when its feature is off, no models dir is set, or the model file is missing. Any other error
(corrupt model, missing ONNX runtime) fails the run. Enable features one at a time to mix real
and fake stages. `--force-fake` ignores the models dir.

Build notes: `stt-whisper` builds whisper.cpp and needs cmake, a C++ toolchain and libclang.
`tts-phonon` needs Rust 1.85 or newer and must be built with `--release`. The input audio is a
synthetic tone, so a real VAD may not detect speech; the JSON then shows
`vad_detected_speech: false` and the whole clip is used. Real STT timing on a tone is still
representative of model compute but not of transcription of real speech.

## Licence

Apache-2.0. See `LICENSE`.
