#!/usr/bin/env bash
# The red-team suite (crates/core/tests/redteam, S2-22) must not be vacuous: it has to LIST at least
# MIN_TESTS runnable tests, every `#[ignore]` in it must be a documented "known gap" that is named in
# docs/redteam.md, and it must really RUN (not just list). Run from the repo root.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
MIN_TESTS=${MIN_TESTS:-80}

list=$(cargo test -p buddy-core --test redteam -- --list 2>/dev/null)
total=$(printf '%s\n' "$list" | grep -c ': test$' || true)
echo "red-team suite lists $total tests (minimum $MIN_TESTS)"
if [ "$total" -lt "$MIN_TESTS" ]; then
  echo "red-team suite is too small ($total < $MIN_TESTS): a case was deleted or the module list shrank"; exit 1
fi

# Every case letter a..q has at least one test module (a_..., b_..., ...).
for f in a_injection b_titles c_feedback d_secure e_deny f_payment g_drift h_hostile_engine \
         i_approval j_delimiters k_huge l_never m_kill n_origin o_audit p_settings q_leaks; do
  n=$(printf '%s\n' "$list" | grep -c "^${f}::" || true)
  if [ "$n" -lt 1 ]; then echo "no tests for case module $f"; exit 1; fi
done

# Ignored tests are known gaps: each must carry `known gap` in its reason and be in docs/redteam.md.
while IFS= read -r line; do
  name=$(printf '%s' "$line" | sed -E 's/^.*fn ([a-z0-9_]+).*$/\1/')
  grep -q "$name" docs/redteam.md || { echo "ignored test $name is not listed in docs/redteam.md"; exit 1; }
done < <(grep -rhE -A1 '#\[ignore *= *"known gap' crates/core/tests/redteam/*.rs | grep -E 'fn [a-z0-9_]+' || true)
if grep -rnE '#\[ignore\]|#\[ignore *= *"' crates/core/tests/redteam/*.rs | grep -v 'known gap'; then
  echo "an ignored red-team test without a 'known gap' reason"; exit 1
fi

# It runs, and everything that is not a known gap passes.
cargo test -p buddy-core --test redteam 2>&1 | tee /dev/stderr | grep -qE '^test result: ok\. [0-9]+ passed; 0 failed'
