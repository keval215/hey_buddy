# Approval window and broker (S2-16)

ADR 0003 section 7 and ADR 0004 are PROVISIONAL (D-006, D-007). This ticket adds the human-facing
side of approvals; it does not touch the policy gate or the firewall (D-008).

## Flow

```
supervisor (S2-23)                core::approval::ApprovalBroker             approval window (Tauri + React)
  gate.request_*_approval ...
  broker.request_plan / request_step ──► id (128-bit random), digest, typed summary
                                        ApprovalUi::show ───────────────────► event approval:request (DTO only)
                                                                              window shows it, then calls
                                        mark_displayed(id) ◄──────────────── approval_displayed(id)  (starts 500 ms guard)
                                        respond(id, decision, echoed digest) ◄ approval_respond(id, decision, echoed_digest)
  PendingApproval::wait() ◄── Approved | Denied | Expired | Killed
  (Approved) -> gate.decide / authorize ... (only the gate creates the sealed capability)
                                        ApprovalUi::close ──────────────────► event approval:closed (window hides)
```

* The broker is in `crates/core/src/approval/` (`mod.rs` broker, `summary.rs` typed summaries and the
  sanitiser, `dto.rs` IPC types, `tests.rs`). The window shell is `apps/desktop/src-tauri/src/approval.rs`,
  the React view `apps/desktop/src/approval/`.
* `wait()` blocks (condvar plus the injected clock, like the gate). Call it from `spawn_blocking`.
  Dropping a `PendingApproval` cancels the request (Denied) and closes its window entry.
* The broker takes typed facts (`StepFacts`: closed `OpKind`, `Tier`, untrusted label/text) and the
  policy digests (`ApprovalDigest::from(&PlanDigest | &ActionDigest)`). It never sees model prose.
* How S2-23 should use it: ask the gate for the approval (its own timeouts of 120 s plan / 60 s step are
  stricter and stay in force), then ask the broker for the same digest and map `Approved` to
  `gate.decide(.., Approve)`, anything else to `Deny` / task end. The broker's ids are its own (random);
  the gate's `ApprovalId` is internal to the supervisor. Pending requests are capped at 16.

## Guarantees

1. **The window can only respond.** There is no command, event or DTO that creates a request. The
   three window commands (`approval_ready`, `approval_displayed`, `approval_respond`) read or answer
   requests that core made; each also checks that the calling window label is `approval`.
2. **The broker does not create the sealed capability.** It returns a decision to its caller. The source
   test `the_broker_does_not_mint_anything` and the CI tripwire (no `ApprovedAction {` outside
   `crates/policy/src/gate.rs`) keep that true.
3. **Fail closed.** Every refused response is "no decision" and the request stays pending (or expires):
   unknown/duplicate/resolved id (`unknown_id`), wrong digest (`digest_mismatch`, exact lowercase hex,
   compared without early exit), Approve before the display acknowledgement (`not_displayed`), Approve
   less than 500 ms after the acknowledgement (`too_early`). A window that cannot be shown, or no window,
   means Denied. Closing the window means Denied. No randomness for the id means no request.
4. **Deny is always accepted** (it still needs the id and digest). Stopping is never dangerous, and the
   window promises "Deny always enabled, Esc = Deny". Only Approve is guarded.
5. **Expiry.** A plan request expires after 10 minutes, a step/Submit request after 30 seconds (the
   "stall" limit). A caller may only tighten that. Expiry is checked by waiters and on every response;
   an answer at the deadline is refused and resolves the request `Expired`.
6. **Kill.** The broker registers on the `KillSwitch`: every pending request resolves `Killed` and the
   window gets `approval:closed`. A response that races the epoch bump (before the hook ran) also
   resolves `Killed`. The window has a Stop-agent button (`kill_agent`, allowed in `approval.json`).
7. **Display data only.** The summary line is numbers and tokens; targets and typed text pass `clean`:
   hostile/control/bidi/zero-width/tag characters removed, whitespace collapsed, `<` `>` become angle
   quotes, `"` becomes `'`, labels cut at 60 characters, typed text at 1000. The page strips hostile
   characters again (`inert`) and renders only text nodes (no raw HTML injection, no autofocus).
8. **Debug and logs carry no content.** `StepFacts`, the DTOs and `PendingApproval` print counts and tokens.

## Window rules

* Separate window `approval`, hidden until a request arrives, always on top, `content_protected`, and on
  Windows excluded from capture with `overlay::protect` (`exclude_window_from_capture`). If exclusion
  fails the window is destroyed and every request is denied.
* The window is focused when shown so Esc and Ctrl+Shift+A work. Nothing inside it has focus. Enter and
  Space never approve: the key handler swallows them, and a click with `detail === 0` (a keyboard-made
  click) is ignored. Approve needs a mouse click or Ctrl+Shift+A, and only 500 ms after the display
  acknowledgement (visible countdown on the button).
* `capabilities/approval.json` allows exactly: `core:event:allow-listen`, `core:event:allow-unlisten`,
  `allow-approval-ready`, `allow-approval-displayed`, `allow-approval-respond`, `allow-kill-agent`.

## UI threat rows to tests

| Threat (ADR 0003 rows 9, 10, 13) | Defence | Test |
|---|---|---|
| Compromised webview answers an approval it never saw / guesses an id | 128-bit random ids; digest echo; id must be outstanding | `unknown_and_duplicate_ids_are_rejected`, `request_ids_are_random_and_unique`, `digest_mismatch_is_rejected_and_the_request_stays_pending` |
| Wrong or other request's digest | exact digest compare, request stays pending | `digest_mismatch_*`, `another_requests_digest_does_not_answer_this_one` |
| Replay / double answer / answer after the end | single use, removed on resolve | `approve_after_resolution_is_rejected`, `unknown_and_duplicate_ids_are_rejected`, `concurrent_responses_exactly_one_wins` |
| Click-through / instant approve | display ack plus 500 ms guard, enforced in core and UI | `approve_within_500_ms_*`, `approve_before_the_window_acknowledged_display_is_rejected`, `a_repeated_display_ack_restarts_the_guard`; vitest `input guard` |
| Keyboard activation of a focused Approve | Enter/Space swallowed, synthetic click ignored | vitest `a keyboard-synthesised click ... never approves`, `Ctrl+Shift+A approves only after the guard` |
| Default focus on Approve | no autofocus anywhere | vitest `has no autofocus anywhere ...`, source check |
| Stale approval (user walked away) | expiry 10 min / 30 s | `plan_expires_after_ten_minutes`, `step_stalls_out_after_thirty_seconds`, `answer_exactly_at_the_deadline_*`, `a_waiting_thread_wakes_*` |
| Approval survives a kill | kill hook plus epoch check | `kill_resolves_every_pending_request_killed_*`, `kill_epoch_moved_without_the_hook_*`; vitest `a Killed close empties the view` |
| Hostile label (RTL override, newline, `<script>`, huge) | `clean` in core, `inert` in the page, text nodes | summary tests (`rtl_override_*`, `script_tags_*`, `huge_text_*`), `hostile_labels_reach_the_window_cleaned`; vitest `renders hostile labels as inert text` |
| Other window answers (settings, overlay, preview) | per-window capability plus label check | desktop tests `approval_capability_grants_exactly_the_allowed_permissions`, `approval_commands_are_only_in_the_approval_capability` |
| Window can mint / approve itself | broker returns a decision only | `the_broker_does_not_mint_anything`, CI tripwire |
| Test doubles in the shipped app | `fakes` / `test-support` never in the desktop tree | `the_desktop_manifest_enables_no_test_doubles`, CI `cargo tree -p desktop -e features` greps |
| Window cannot be hidden from capture | window destroyed, requests denied | `a_window_that_cannot_show_means_denied`, `no_window_attached_means_denied` (capture exclusion itself: manual) |

## Last-request view

Agent planning requests (`purpose: AgentPlan`) show the line "This request contains the task you
spoke" in the shared request view (preview panel and settings "Last request"), because the planner
prompt includes the spoken task text.

## Not tested on this machine (Windows only)

* The window is really excluded from screen capture (`exclude_window_from_capture`, WDA_EXCLUDEFROMCAPTURE).
* Always on top, focus behaviour on show, Esc and Ctrl+Shift+A reaching the window.
* The mouse-click guard with a real mouse, the Stop-agent button and the hotkey while the window is up.
* Keyboard-only path. All of these are listed in `docs/manual-tests.md` ("Approval window (S2-16)").
* Nothing in this ticket adds `cfg(windows)` code; the capture exclusion reuses `overlay::protect`.
