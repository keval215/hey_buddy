# Contributing

Thanks for helping. A few rules keep the project safe and legally clean.

## Ground rules

- Privacy first, fail closed: on any firewall error, block capture.
- No secrets, keys or tokens in code, tests, fixtures or logs.
- No network calls in tests; use fakes. No model downloads in tests.
- Screenshots and audio are never written to disk outside explicit debug mode.
- Platform code goes behind `#[cfg(target_os = ...)]` and every platform trait has a fake.
- Dependencies must be one of MIT, Apache-2.0, BSD, ISC, Zlib, MPL-2.0, Unlicense, CC0-1.0, 0BSD (`cargo deny check licenses`).
  Non-commercial models are never bundled.
- Contributions are licensed under Apache-2.0.

## Before opening a PR

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm lint && pnpm typecheck && pnpm test
```

Architecture decisions live in `docs/adr/`; read ADR 0001 for the crate graph.
