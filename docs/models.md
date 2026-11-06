# Model registry: adding and verifying entries (S3-5)

`models/registry.json` is the catalog of models the app knows. Every `url` and `sha256` is currently `TODO`: the app
shows such models as "no download source yet" and cannot download them. **Never invent a URL or a checksum.** Filling
them in is a maintainer step (BLOCKED-ON-USER in the step plan) that needs a person to pick the official download
location and read the licence.

## What `TODO` means

- `url: "TODO"` / `sha256: "TODO"`: no verified source yet. The model is listed but cannot be downloaded. Users can still
  place the file in the models folder by hand.
- A URL and its checksum are filled in **together**. The CI check fails on a real URL with a `TODO` checksum, and on a
  checksum without a URL. (The app itself can download a file with a `TODO` checksum only after an extra confirm, and then
  shows it as `unverified`; do not rely on that for the shipped registry.)
- `size`, `languages`, `licence` may also hold `TODO...` text. `licence` values starting `TODO-verify` mean the licence
  has not been read yet.

## Adding or filling in an entry

1. Pick the official download location (the model author's release page or model hub). It must be an `https://` URL
   with no credentials or tokens in it. Plain `http`, `file:` and `user:pass@` URLs are refused by the app and by CI.
2. Run the verify tool yourself (explicit run; nothing in the app or CI calls it, and it downloads the whole file, which
   can be gigabytes):

   ```
   cargo run -p registry-verify -- --id whisper-small --url https://<official host>/<path>/ggml-small.bin
   ```

   Flags: `--max-bytes N` (default 8 GiB; the download stops with `too_large` above it), `--registry <file>` (default
   `models/registry.json`, read only). It downloads with the app's own downloader (HTTPS only, redirects re-checked,
   size cap) into a temp folder, removes the folder, and prints `size_bytes`, `sha256` and a licence-file hint. It
   **never edits the registry**.
3. Read the licence file the tool points at, and check it matches the `licence` field. Fix `licence` if it is wrong.
4. Paste `url`, `sha256` (64 lowercase hex characters) and `size_bytes` by hand. `size_bytes` gives the app a size sanity
   limit and the disk-space check.
5. Run `cargo run -p registry-check` (and `cargo test -p voice`). Commit the change with the verification noted in the PR.

## Directory and multi-file models (phonon, kokoro)

Models that are a folder of files use a `files` list; the entry-level `url` and `sha256` must then stay `TODO`:

```json
"files": [
  {"path": "phonon-31m/model.safetensors", "url": "https://...", "sha256": "<64 hex>", "size_bytes": 123},
  {"path": "phonon-31m/config.json",       "url": "https://...", "sha256": "<64 hex>"}
]
```

`path` is relative to the models folder, `/` separated, and each component may only use `A-Z a-z 0-9 . _ -` (no `..`,
absolute paths, drive letters, backslashes, `:` or Windows device names). Run `registry-verify` once per file. The app
downloads all files into a staging folder, verifies each, and only then moves them into place.

## Licence display rules (D-004)

- The download UI shows each model's licence (the `licence` field) before the user clicks Download.
- `commercial_ok` is **information only**. It never blocks a download or a load; models are optional user downloads and
  weights are never committed to the repo.
- Code dependencies are a separate matter (permissive licences only; see `CLAUDE.md`).

## CI

`registry-check` runs in CI on Linux: schema, unique ids, every profile reference exists, a `default` profile exists, at
least 10 entries (so it cannot pass on an empty file), URLs are `https://` or `TODO`, and URL/sha256 are set together.
