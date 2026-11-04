#!/usr/bin/env bash
# Stable rustdoc does not enforce the EXXXX in ```compile_fail,EXXXX. This script compiles every
# such snippet in crates/policy/src/{gate,lib}.rs with rustc against the built `policy` rlib and
# checks that the stated error code is really reported. Run from the repo root.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
cargo build -p policy >/dev/null
deps=target/debug/deps
rlib=$(ls -t "$deps"/libpolicy-*.rlib | head -1)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
n=0
for f in crates/policy/src/gate.rs crates/policy/src/lib.rs; do
  # Split the //! doc blocks into one file per compile_fail snippet: "<code> <index>.rs".
  awk -v dir="$tmp" -v pre="$(basename "$f" .rs)" '
    /^\/\/! ```compile_fail,E[0-9]+/ { match($0, /E[0-9]+/); code=substr($0, RSTART, RLENGTH); n++; out=dir "/" pre "_" n "_" code ".rs"; print "fn main() {" > out; inblock=1; next }
    inblock && /^\/\/! ```/ { print "}" >> out; close(out); inblock=0; next }
    inblock { sub(/^\/\/! ?/, ""); print >> out }
  ' "$f"
done
for s in "$tmp"/*.rs; do
  code=$(basename "$s" .rs | sed 's/.*_//')
  n=$((n + 1))
  if out=$(rustc --edition 2021 --crate-type bin --emit=metadata -o "$tmp/out" \
        --extern "policy=$rlib" -L "dependency=$deps" "$s" 2>&1); then
    echo "FAIL (compiled): $(basename "$s")"; fail=1
  elif ! grep -q "error\[$code\]" <<<"$out"; then
    echo "FAIL (wrong code, wanted $code): $(basename "$s")"; echo "$out" | grep -E '^error' | head -3; fail=1
  fi
done
echo "checked $n compile_fail snippets"
exit $fail
