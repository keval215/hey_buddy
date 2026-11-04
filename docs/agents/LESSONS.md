# Lessons

_Owner: orch. Short, reusable facts that save future rounds. Remove entries
that stop being true._

Format: `- L-000 <area>: <lesson> (from T<n>)`

- L-001 env: Agents run on Linux in the cloud; macOS/Windows code must compile behind `cfg` and be listed for the user to test locally. (setup)
- L-002 tests: Never download models or call networks in tests; skip cleanly when a model file is missing. (setup)
- L-003 cargo: Never name a package `core`, `std` or `alloc`; our `crates/core` is package `buddy-core` (use `-p buddy-core`). (T1)
- L-004 ci: Cross-platform jobs need the same prebuild steps as Linux (frontend dist for Tauri `generate_context!`, icon.ico on Windows). `cargo test --workspace` includes desktop and needs webkit2gtk-4.1 + GTK3 on Linux. (T1)
- L-005 registry: Unverified licence strings use a visibly non-final value (`TODO-verify-...`); invariants JSON Schema can't express live in the loader with tests. (T2)
- L-006 voice: `fakes` feature may pull optional tokio; only enable via dev-dependencies, verify with `cargo tree -p desktop -e features | grep fakes`. (T3)
- L-007 tests: model-gated tests skip only on ModelMissing/unset env; any other load error must fail. (T4)
- L-008 deps: check each crate's licence on crates.io (whisper-rs is Unlicense). stt-whisper needs cmake + C++ + libclang; mic-cpal needs libasound2-dev on Linux. (T4)
- L-009 deps: pin `default-features=false` when defaults pull in networking; check dep edition/MSRV vs workspace rust-version; `cargo deny --all-features` needed to cover optional deps. (T5)
- L-010 config: test fallback paths for user-config lists (empty result, corrupt, unreadable, re-added default); never forward OS text/window titles into errors; unresolvable window identity fails the whole capture. (T7)
- L-011 tests: compile_fail doctests must pin an error code (`compile_fail,E0xxx`) with todo!() placeholders. Cross-check apple targets with `--lib` (Swift build scripts in dev-deps fail on Linux). OS accessibility values NaN/inf/zero are errors, not empty rects. (T8)
- L-012 sanitisers: for LLM-prompt text strip ALL Unicode format chars (tag chars U+E0000-E007F, variation selectors, soft hyphen, invisible operators), not only bidi/zero-width; re-enforce source contracts geometrically (nodes over secure rects), not only by tree depth. (T9.1)
- L-013 llm: reqwest `rustls-no-provider` panics without an installed CryptoProvider (llm installs ring once); cross-clippy ring crates needs a C cross-compiler (use `--features keychain --lib` only); anchor tree greps as `(openssl|openssl-sys|native-tls) v`; rustdoc accepts a wrong E-code sometimes — verify with --nocapture. (T9)
- L-014 memory: NEVER write `open(f,'w').write(open(f).read()...)` — the 'w' truncates before the read (this wiped HANDOFFS.md once; restored from git). Read into a variable first. (orch)
- L-015 tests: pair every 'X absent from logs' assertion with a 'log capture alive' assertion; don't use a (0,0) origin in coordinate-mapping tests; push-to-talk needs a bounded recovery when a release event is lost. (T12)
