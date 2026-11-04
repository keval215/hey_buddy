# Policy crate, part 1 (S2-11a)

Crate `crates/policy` implements ADR 0003 sections 4 and 5 as pure code: the `plan/v1` wire schema and its strict re-validation, provenance, the sealed `ValidatedPlan` with its digest, and the tier table as the function `classify`. ADR 0003 is provisionally approved (D-006); where the user's autonomous-mode defaults differ from it, the stricter rule wins.

Dependencies: `action`, `firewall`, `serde`, `serde_json`, `sha2`, `thiserror`. CI fails if the tree contains windows/objc2/core-foundation/llm/capture/voice/driver/tokio/reqwest.

## Not here yet (S2-11b)
The gate (`evaluate`, `authorize`), plan and step grants, approvals, the sealed `ApprovedAction`, observe mode, kill epoch, drift rules, audit. Row 27 (frozen plan, `out_of_plan`) is a gate rule: only `ReasonCode::OutOfPlan` exists here and `classify` never returns it. The dry-run bindings type also arrives in S2-11b; `plan_digest` takes their canonical bytes as `&[u8]`.

## Plan schema `plan/v1` (`src/plan.rs`)
`{"steps":[{"op":..., ...}], "cannot": null | "<code>"}` (`cannot` may be omitted).

| op | fields |
|---|---|
| `observe`, `copy_selection` | none |
| `find_text`, `answer_from_screen` | `query` 1..=200 chars |
| `scroll` | `direction` up/down, `amount` 1..=10 |
| `focus_window`, `open_app` | `app` 1..=64 chars |
| `click`, `submit` | `target` |
| `type` | `target`, `text` 1..=1000 chars (ADR said 2000; `action::TypedText` caps at 1000, stricter wins) |
| `key_combo` | `keys`: 1..=4 tokens from a closed vocabulary |

`target` = `{role?, label 1..=120, nth? 1..=5}`. Roles are the `firewall::NodeRole` names, snake_case (`text_field`) or the firewall display name (`text field`). There is no tier, approval, url, path, argument or command field; anything else is `unknown_field` / `unknown_op` (table row 25).

### Key chords
Tokens (case-insensitive): modifiers `ctrl alt shift win` (aliases control, windows, meta, super), keys `enter esc tab up down left right home end pageup pagedown space backspace delete insert printscreen a-z 0-9 f1-f12`. A chord equal to a closed `action::KeyCombo` becomes `PlanStep::KeyCombo`; Ctrl+C becomes `PlanStep::CopySelection`; every other chord (Ctrl+V, Alt+F4, Win+*, Ctrl+Enter ...) becomes `PlanStep::KeyChord(UnknownChord)`. It is representable so the user sees it refused, but there is no conversion to an `action::Action`.

## Validation order (`plan::validate` / `validate_bytes`)
First failure wins; every error is a fixed token (`PlanError::code()`), never input.
1. size <= 16 KiB (`draft_too_large`), 2. valid UTF-8, 3. no NUL byte, 4. JSON nesting depth <= 5 (bracket pre-scan, before any parsing), 5. JSON syntax and duplicate keys (a custom serde visitor rejects duplicates, including `\u`-escaped spellings), 6. unknown fields at every level, 7. `cannot` (`steps: []` + a code is `cannot_plan`; a code with steps is `bad_cannot`), 8. 1..=12 steps, 9. per step: op, field types, string rules, ranges, key tokens, `type.text` through `action::TypedText::new`, 10. app-name provenance, then catalogue resolution.

Strings are never cleaned silently: any `firewall::is_hostile_char` character rejects the plan (text, labels, queries, app names, key tokens). Accepted labels, queries and app names are then sanitised like screen text (`<` `>` become `‹` `›`, `"` becomes `'`, whitespace collapsed). Matching uses the sanitised label only.

`PlanContext{transcript, catalogue, limits}`; limits can only tighten (clamped to the hard caps).

## Provenance
- `open_app` / `focus_window`: the normalised app name (lowercase, accents folded, invisibles stripped, whitespace collapsed) must appear in the normalised transcript as whole words, else `app_not_in_request`. Then `Catalogue::resolve(name)` must return exactly one entry (display name to `LaunchId` + `AppId`), else `unknown_app`. Provenance is checked first so model-invented names never probe the catalogue. `StaticCatalogue` is the fake; ambiguous display names resolve to nothing.
- `type.text`: a whole-word normalised substring of the transcript is `Provenance::FromRequest`, otherwise `ModelGenerated` (row 13: Consequential, never batchable). Text that is empty after normalising is `ModelGenerated`.

## Canonical bytes and digest
See the module docs of `plan.rs` for the byte layout. `ValidatedPlan::plan_digest(nonce, task, bindings)` = `sha256("hey_buddy/plan/v1" ‖ lp(nonce) ‖ u64 task ‖ lp(canonical plan) ‖ lp(bindings))`. The canonical bytes are core's re-serialisation of the typed plan (model key order, spelling, whitespace and aliases do not matter). `ValidatedPlan` has private fields, a `pub(crate)` constructor, no `Clone`/`Default`/serde/`From`, and `Debug` prints the 8-hex content id and the step count only. Golden vectors in `tests/digest.rs` were computed with an independent Python encoder.

## Tier classification (`src/classify.rs`)
`classify(&BoundStepFacts, &PolicyContext) -> Classification{tier, submit, batchable, reason}`. The tier is the maximum over matching rows; facts only raise. `BoundStepFacts::new(op)` starts with everything unknown; a non-Read step with a missing environment fact (app id, denied, own process, integrity, restricted, payment context, session; plus `secure` for click/type/submit and `focused_secure` for keyboard-like ops) is Never with reason `missing_fact`. Unknown role is Consequential. Reads need no environment facts.

| Row | Rule | Where |
|---|---|---|
| 1 | observe: Read, batchable (a paused/blocked session keeps it Read, it then fails by itself) | `baseline` `Observe`; `tests/classify.rs::row01_*` |
| 2, 3 | find_text, answer_from_screen: Read | `baseline`; `row02_*`, `row03_*` |
| 4, 5, 6 | scroll, focus_window, open_app: Reversible, batchable | `baseline`; `row04_*` to `row06_*` |
| 7 | click on Tab, TextField, editable ComboBox: Reversible | `baseline` `Click`; `row07_*` |
| 8 | click, anything else or unknown role: Consequential, not batchable | `baseline` `Click`; `row08_*` |
| 9 | `submit` op, or click/submit with a submit-lexicon label: Consequential + submit, not batchable | `baseline` `Submit`, `fact_rows` lexicon block; `row09_*` |
| 10, 11 | destructive / purchase label on click or submit: Never | `fact_rows` lexicon block; `row10_*`, `row11_*` |
| 12 | type, text from the request, editable target: Consequential, batchable | `baseline` `Type`; `row12_*` |
| 13 | type, model-generated text: Consequential, not batchable | `baseline` `Type`; `row13_*` |
| 14 | secure target, focus in secure field, Luhn text, secret/payment label: Never | `fact_rows`, `lexicon::looks_like_card_number`; `row14_*` |
| 15 | arrows, PgUp/PgDn, Home/End, Tab, Shift+Tab, Esc: Reversible | `baseline` `Key`; `row15_*` |
| 16 | Ctrl+A, copy_selection (Ctrl+C): Reversible; Never if focus in a secure field | `baseline`, `fact_rows`; `row16_*` |
| 17 | Enter, Ctrl+Enter: Consequential + submit | `baseline`; `row17_*` |
| 18 | Ctrl+V/S/Z/Y/W, Alt+F4, Delete, Backspace, Alt+Tab, Space, Ctrl+F, any other chord: Consequential | `baseline`; `row18_*` |
| 19 | any Win chord, Ctrl+Alt+Del, Ctrl+Shift+Esc, Delete / Shift+Delete in explorer: Never | `baseline` `Chord`, `fact_rows`; `row19_*` |
| 20 | denied app: Never | `fact_rows`; `row20_*` |
| 21 | session Paused/Blocked: Never (except observe) | `fact_rows`; `row21_*` |
| 22 | own process: Never | `fact_rows`; `row22_*` |
| 23 | payment context, any non-Read: Never | `fact_rows`; `row23_*` |
| 24 | Never-apps (`NeverApps`), elevated window, restricted app: Never | `never_apps.rs`, `fact_rows`; `row24_*` |
| 25 | not representable in plan/v1 | `plan.rs` closed schema; `plan.rs::schema_is_closed`, `row25_*` |
| 26 | unknown action kind (`FactOp::Unknown`): Consequential, not batchable | `baseline`; `row26_*` |
| 27 | step is not the next frozen step: Deny `out_of_plan` | gate (S2-11b); only `ReasonCode::OutOfPlan` here, `row27_*` |

Properties (`tests/classify.rs`): `prop_a` a label never lowers a tier below the label-less baseline; `prop_b` Never rows hold under every combination of the other facts; `prop_c` missing facts give Never (or Consequential for an unknown role); `prop_d` batchable only for rows 1-7, 12, 15, 16, never with submit or Never. `lexicon_bypass_table` covers case, hyphens, ellipsis, zero-width/bidi/tag characters inside words, accents, "Deleted" vs "Delete", and other languages (they fall to the role baseline).

### Stricter readings of ADR 0003 section 5 (chosen where the table is ambiguous)
1. Rows 10 and 11 also apply to the `submit` op (the table says `click`).
2. Row 14: a secure target is Never for click, type and submit (the table says `type`); focus in a secure field is Never for every keyboard-like op (type, submit, key, chord, copy, unknown). A click that only focuses a sensitive-labelled text field is Never.
3. Row 24: the Never-app list and elevation apply to Read ops too; restricted apps (ADR 0004 section 8) stay observable. The Never-app list also holds script hosts (wscript, cscript, mshta, rundll32, regsvr32, wsl, bash), openconsole/WindowsTerminal and the auth surfaces, and the installer heuristics of ADR 0004 section 8.
4. Row 12 requires a known-editable target; otherwise the type step is asked for every time (not batchable).
5. Row 21: an unknown `session` fact is Never (`missing_fact`) for EVERY op except `observe`, Read ops `find_text` / `answer_from_screen` included. Other environment facts stay optional on Read ops.
6. The submit lexicon also contains `signin`, the purchase lexicon `check out` (hyphen/space spellings), a label such as "Next" on a Tab raises it to Consequential + submit.
7. `type.text` provenance and app-name provenance are whole-word matches (a bare substring such as "note" in "notepad" does not count).
8. Plan `type.text` is capped at 1000 characters, not 2000.
9. Ctrl+C typed as `key_combo` is normalised to `copy_selection`.
10. `open_app` / `focus_window` check the TARGET app: `BoundStepFacts::for_step` seeds `target_app` from the catalogue entry; the Never-app list and installer heuristics apply to it as well as to `app_id`, and the caller supplies `target_denied` (the target is not deny-listed, restricted, elevated or ours). Unknown `target_app` / `target_denied` is Never (`missing_fact`); `Some(true)` is Never (`denied_app`).
11. Row 19: in explorer and the other listed file managers, Ctrl+D and any chord whose key is Delete (Shift+Delete, Ctrl+Shift+Delete ...) are Never. Row 17: any chord containing Enter (Ctrl/Shift/Alt+Enter) is Consequential + submit.
12. Card detection folds common Unicode decimal digits (fullwidth, Arabic-Indic, Devanagari, mathematical ...) to 0-9 and accepts whitespace (incl. NBSP) and `- . / _` (and Unicode hyphens) between digits; Luhn still has to pass, so ordinary long order numbers are not flagged unless they happen to pass Luhn.

### Known limitations
- Lexicons are English-only heuristics that only escalate: labels in other languages, homoglyphs (Cyrillic look-alikes) and payment pages without recognisable labels fall to the role baseline.
- The firewall's screen-text sanitiser turns invisible characters inside a label into spaces (`Del<ZWSP>ete` becomes `Del ete`), so a label that reaches policy through that path no longer matches a lexicon word. `BoundStepFacts::label` strips instead of spacing, but it cannot undo what the firewall already did; the baseline (Consequential with its own approval) still applies. Follow-up: have the firewall expose the lexicon verdicts for the unsplit label (like `sensitive_label`).
- `payment_context`, `secure`, `sensitive_label` etc. are facts the caller must derive from the firewall observation; `assume_benign_environment` exists for fakes and tests only and is compiled only under `cfg(any(test, feature = "test-support"))`; the `test-support` feature is off by default, only the crate's own tests enable it (dev-dependency self-reference) and CI greps that nothing else uses it.
- Known limitation until S2-14e: the joined-label flags (lexicon verdicts for the unsplit label, see above) are not yet exposed by the firewall; labels that arrive pre-split by the firewall sanitiser can miss the lexicons. The target-app verdict (`target_denied`) is likewise caller-supplied until the driver/catalogue facts exist.
