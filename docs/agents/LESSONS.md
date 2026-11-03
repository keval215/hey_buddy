# Lessons

_Owner: orch. Short, reusable facts that save future rounds. Remove entries
that stop being true._

Format: `- L-000 <area>: <lesson> (from T<n>)`

- L-001 env: Agents run on Linux in the cloud; macOS/Windows code must compile behind `cfg` and be listed for the user to test locally. (setup)
- L-002 tests: Never download models or call networks in tests; skip cleanly when a model file is missing. (setup)
- L-003 cargo: Never name a package `core`, `std` or `alloc`; our `crates/core` is package `buddy-core` (use `-p buddy-core`). (T1)
- L-004 ci: Cross-platform jobs need the same prebuild steps as Linux (frontend dist for Tauri `generate_context!`, icon.ico on Windows). `cargo test --workspace` includes desktop and needs webkit2gtk-4.1 + GTK3 on Linux. (T1)
- L-005 registry: Unverified licence strings use a visibly non-final value (`TODO-verify-...`); invariants JSON Schema can't express live in the loader with tests. (T2)
