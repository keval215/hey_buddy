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

## Licence

Apache-2.0. See `LICENSE`.
