#!/usr/bin/env bash
# Stable rustdoc does not enforce the EXXXX in ```compile_fail,EXXXX. This script compiles every
# such snippet in the sealed-type crates (policy: gate.rs, lib.rs; driver: lib.rs, driver.rs,
# screen.rs) with rustc against the built rlibs and checks that the stated error code is really
# reported. Run from the repo root (needs cargo, jq and rustc).
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# One cargo invocation provides one consistent set of rlibs (policy, driver and the crates and
# dev-dependencies the snippets name).
cargo test -p policy -p driver --no-run --message-format=json 2>/dev/null >"$tmp/artifacts.json" \
  || { cargo test -p policy -p driver --no-run >&2; exit 1; }
rlib_of() {
  jq -r --arg n "$1" 'select(.reason=="compiler-artifact" and .target.name==$n and (.target.kind | index("lib")) and (.profile.test | not)) | .filenames[] | select(endswith(".rlib"))' \
    "$tmp/artifacts.json" | tail -1
}
externs=()
for c in policy driver action firewall frame serde; do
  r=$(rlib_of "$c")
  if [ -n "$r" ]; then externs+=(--extern "$c=$r"); fi
done
deps=target/debug/deps
fail=0
n=0
for f in crates/policy/src/gate.rs crates/policy/src/lib.rs \
         crates/driver/src/lib.rs crates/driver/src/driver.rs crates/driver/src/screen.rs; do
  # Split the //! and /// doc blocks into one file per compile_fail snippet: "<code> <index>.rs".
  awk -v dir="$tmp" -v pre="$(echo "$f" | tr '/.' '__')" '
    /^[ \t]*\/\/[\/!] ```compile_fail,E[0-9]+/ { match($0, /E[0-9]+/); code=substr($0, RSTART, RLENGTH); n++; out=dir "/" pre "_" n "_" code ".rs"; print "fn main() {" > out; inblock=1; next }
    inblock && /^[ \t]*\/\/[\/!] ```/ { print "}" >> out; close(out); inblock=0; next }
    inblock { sub(/^[ \t]*\/\/[\/!] ?/, ""); print >> out }
  ' "$f"
done
for s in "$tmp"/*.rs; do
  code=$(basename "$s" .rs | sed 's/.*_//')
  n=$((n + 1))
  if out=$(rustc --edition 2021 --crate-type bin --emit=metadata -o "$tmp/out" \
        "${externs[@]}" -L "dependency=$deps" "$s" 2>&1); then
    echo "FAIL (compiled): $(basename "$s")"; fail=1
  elif ! grep -q "error\[$code\]" <<<"$out"; then
    echo "FAIL (wrong code, wanted $code): $(basename "$s")"; echo "$out" | grep -E '^error' | head -3; fail=1
  fi
done
echo "checked $n compile_fail snippets"
exit $fail
