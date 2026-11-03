# hey_buddy

An open-source, local-first desktop AI buddy. It sits next to your cursor, sees your screen
when you press a hotkey, talks with you by voice, and (later) runs background agents that
operate your apps safely.

**The desktop buddy that can't see your passwords, runs its voice on your own machine, and
asks before it acts.**

## Status

Early development (Step 1: voice + screen-aware buddy). The repo currently contains the
workspace scaffold only: empty Rust crates, a Tauri 2 + React app shell and CI. Nothing is
usable yet. See `docs/STEP1.md` for the plan and `docs/adr/` for architecture decisions.

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

On Linux the desktop crate needs the Tauri system libraries (webkit2gtk 4.1, GTK 3,
libayatana-appindicator, librsvg). Without them, plain `cargo build`/`cargo test` skip the
desktop crate (it is not a default member); `--workspace` includes it.

## Latency benchmark

`tools/bench` measures speech end to first audio: synthetic audio, then VAD and segmenter, STT,
a fake LLM (scripted deltas with artificial delay), sentence chunking, TTS, and a timestamped
fake audio sink. It prints a table and writes `bench-results.json` (git-ignored). The JSON holds
timings, which stages were real or fake, and host os/arch. It never holds audio or text.

Targets: under 1.0 s on Apple silicon, under 1.5 s on CPU laptops. The pass/fail flag is only
meaningful (and labelled so) when both STT and TTS are real. The LLM is always a fake, so the
number excludes real model time. Segmenter hangover time is not counted: the clock starts when
speech end is detected.

Without models (fakes only; works anywhere, no cmake or audio device needed):

```
cargo run --release -p bench
cargo run --release -p bench -- --out my-results.json --stt-delay-ms 300 --tts-delay-ms 200
```

With models on your own Mac or Windows machine. Put the files in a models dir:
`<dir>/silero-vad-v5.onnx`, `<dir>/whisper-large-v3-turbo.bin` (or another id via `--stt-model`),
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
