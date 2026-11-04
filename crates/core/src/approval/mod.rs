//! The approval broker (ADR 0003 section 7, S2-16): the core side of the approval window.
//!
//! Core code (the supervisor, S2-23) asks the broker for a PLAN approval or a STEP / Submit
//! approval and awaits a typed result: [`ApprovalOutcome`] `Approved`, `Denied`, `Expired` or
//! `Killed`. The window can only RESPOND to a request core created: `approval_respond(id,
//! decision, echoed_digest)` is accepted for an outstanding id only.
//!
//! What the broker is NOT: it never creates the sealed capability that lets the driver act. It
//! hands the human's decision back to the caller, which then goes through the policy gate
//! (`PolicyGate::decide` / `authorize`); only the gate can create that capability. The UI also
//! has no way to create requests: there is no command, event or DTO that does.
//!
//! Fail closed. Every rejected response is "no decision": the request stays pending until the
//! human answers properly, it expires or the kill switch fires.
//!
//! * unknown, repeated or already resolved id: `unknown_id`
//! * wrong echoed digest: `digest_mismatch`
//! * Approve before the window acknowledged displaying the request: `not_displayed`
//! * Approve less than [`INPUT_GUARD`] (500 ms, gate clock) after the display acknowledgement:
//!   `too_early` (Deny is always accepted: stopping is never dangerous)
//! * past the deadline, or the kill epoch moved: the request resolves `Expired` / `Killed` and
//!   the response is refused.
//!
//! Time comes from an injected [`policy::Clock`] only (ManualClock in tests). The kill switch
//! resolves every pending request `Killed` and tells the window to close.
//!
//! Locks: all state sits behind one mutex; the window (UI sink) is called after it is released
//! (L-013). Debug of requests and DTOs never prints content (L-022).

mod dto;
mod summary;

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use policy::{ActionDigest, Clock, KillReason, KillSwitch, PlanDigest};

pub use dto::{
    ApprovalClosedDto, ApprovalDecisionDto, ApprovalKindDto, ApprovalLineDto, ApprovalOutcomeDto,
    ApprovalRequestDto, ApprovalTierDto, APPROVAL_WINDOW, EVT_APPROVAL_CLOSED,
    EVT_APPROVAL_REQUEST,
};
pub use summary::{
    clean, line, live_offer_summary, plan_lines, plan_summary, step_summary, OpKind, StepFacts,
    MAX_LABEL_CHARS, MAX_LINES, MAX_TEXT_CHARS,
};

/// An approve this soon after the window showed the request is ignored (ADR 0003 section 7).
pub const INPUT_GUARD: Duration = Duration::from_millis(500);
/// A plan approval waits at most this long.
pub const PLAN_TTL: Duration = Duration::from_secs(10 * 60);
/// A step approval (and so a stalled run) waits at most this long.
pub const STEP_TTL: Duration = Duration::from_secs(30);
/// Most requests outstanding at once (a flood is refused, not queued).
pub const MAX_PENDING: usize = 16;

/// The typed result the asking code awaits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    /// The human said no, closed the window, or the window could not be shown (fail closed).
    Denied,
    Expired,
    Killed,
}

impl ApprovalOutcome {
    pub fn code(self) -> &'static str {
        match self {
            ApprovalOutcome::Approved => "approved",
            ApprovalOutcome::Denied => "denied",
            ApprovalOutcome::Expired => "expired",
            ApprovalOutcome::Killed => "killed",
        }
    }
    fn dto(self) -> ApprovalOutcomeDto {
        match self {
            ApprovalOutcome::Approved => ApprovalOutcomeDto::Approved,
            ApprovalOutcome::Denied => ApprovalOutcomeDto::Denied,
            ApprovalOutcome::Expired => ApprovalOutcomeDto::Expired,
            ApprovalOutcome::Killed => ApprovalOutcomeDto::Killed,
        }
    }
}

/// Why a response was not accepted (fixed tokens, never input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespondError {
    UnknownId,
    DigestMismatch,
    NotDisplayed,
    TooEarly,
    /// The request had expired; it is now resolved `Expired`.
    Expired,
    /// The kill switch fired; it is now resolved `Killed`.
    Killed,
}

impl RespondError {
    pub fn code(self) -> &'static str {
        match self {
            RespondError::UnknownId => "unknown_id",
            RespondError::DigestMismatch => "digest_mismatch",
            RespondError::NotDisplayed => "not_displayed",
            RespondError::TooEarly => "too_early",
            RespondError::Expired => "expired",
            RespondError::Killed => "killed",
        }
    }
}

impl std::fmt::Display for RespondError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for RespondError {}

/// Why a request could not be created (fixed tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// No secure randomness for an unguessable id: nothing is shown.
    NoRandomness,
    /// Too many outstanding requests.
    TooMany,
}

impl RequestError {
    pub fn code(self) -> &'static str {
        match self {
            RequestError::NoRandomness => "no_randomness",
            RequestError::TooMany => "too_many",
        }
    }
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for RequestError {}

/// The window the broker shows requests in (the Tauri approval window; a recorder in tests).
pub trait ApprovalUi: Send + Sync {
    /// Show the request. `false` = it could not be shown: the request is Denied.
    fn show(&self, request: &ApprovalRequestDto) -> bool;
    /// The request ended (any outcome): close it.
    fn close(&self, closed: &ApprovalClosedDto);
}

/// No window: nothing can be shown, so every request is denied (fail closed).
pub struct NoApprovalUi;
impl ApprovalUi for NoApprovalUi {
    fn show(&self, _request: &ApprovalRequestDto) -> bool {
        false
    }
    fn close(&self, _closed: &ApprovalClosedDto) {}
}

/// The digest a request is bound to (full lowercase hex). Built from the policy digests; the
/// window echoes it back and the broker compares it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalDigest(String);

impl ApprovalDigest {
    /// 64 lowercase hex digits, else `None`.
    pub fn from_hex(h: &str) -> Option<Self> {
        let ok = h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        ok.then(|| Self(h.to_string()))
    }
    pub fn hex(&self) -> &str {
        &self.0
    }
    /// First 8 hex digits.
    pub fn short(&self) -> &str {
        &self.0[..8]
    }
}

impl From<&PlanDigest> for ApprovalDigest {
    fn from(d: &PlanDigest) -> Self {
        Self(d.hex())
    }
}

impl From<&ActionDigest> for ApprovalDigest {
    fn from(d: &ActionDigest) -> Self {
        Self(d.hex())
    }
}

impl std::fmt::Debug for ApprovalDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApprovalDigest({})", self.short())
    }
}

struct Entry {
    dto: ApprovalRequestDto,
    created_seq: u64,
    expires: Duration,
    displayed: Option<Duration>,
    epoch: u64,
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Entry>,
    resolved: HashMap<String, ApprovalOutcome>,
    next_seq: u64,
}

struct Inner {
    clock: Arc<dyn Clock>,
    kill: KillSwitch,
    ui: Mutex<Arc<dyn ApprovalUi>>,
    state: Mutex<State>,
    cv: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Asks the human; see the module docs.
#[derive(Clone)]
pub struct ApprovalBroker {
    inner: Arc<Inner>,
}

type Closed = Vec<(String, ApprovalOutcome)>;

impl ApprovalBroker {
    /// A broker with no window attached (every request is denied until [`Self::set_ui`]).
    /// Registers a hook on `kill` so a kill resolves every pending request `Killed`.
    pub fn new(clock: Arc<dyn Clock>, kill: &KillSwitch) -> Self {
        let inner = Arc::new(Inner {
            clock,
            kill: kill.clone(),
            ui: Mutex::new(Arc::new(NoApprovalUi)),
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        kill.on_trip(Arc::new(move |_reason: KillReason| {
            if let Some(inner) = weak.upgrade() {
                ApprovalBroker { inner }.kill_all();
            }
        }));
        Self { inner }
    }

    /// The app's broker: real clock, stopped by the app's kill switch.
    pub fn with_system_clock(kill: &KillSwitch) -> Self {
        Self::new(Arc::new(policy::SystemClock::new()), kill)
    }

    pub fn set_ui(&self, ui: Arc<dyn ApprovalUi>) {
        *lock(&self.inner.ui) = ui;
    }

    fn ui(&self) -> Arc<dyn ApprovalUi> {
        Arc::clone(&lock(&self.inner.ui))
    }

    /// Ask for a PLAN approval. `steps` are typed facts; the digest is the plan digest the
    /// window must echo back. `ttl` can only tighten [`PLAN_TTL`].
    pub fn request_plan(
        &self,
        digest: &ApprovalDigest,
        steps: &[StepFacts],
        live: bool,
        ttl: Option<Duration>,
    ) -> Result<PendingApproval, RequestError> {
        let lines = summary::plan_lines(steps);
        let summary = summary::plan_summary(digest.short(), &lines);
        self.submit(
            ApprovalKindDto::Plan,
            digest.hex().to_string(),
            summary,
            lines,
            live,
            false,
            PLAN_TTL,
            ttl,
        )
    }

    /// Ask "Run it for real?" after a dry run (S2-23): a PLAN approval for the same digest with a
    /// distinct summary. `ttl` can only tighten [`PLAN_TTL`].
    pub fn request_live_offer(
        &self,
        digest: &ApprovalDigest,
        steps: &[StepFacts],
        ttl: Option<Duration>,
    ) -> Result<PendingApproval, RequestError> {
        let lines = summary::plan_lines(steps);
        let summary = summary::live_offer_summary(digest.short(), &lines);
        self.submit(
            ApprovalKindDto::Plan,
            digest.hex().to_string(),
            summary,
            lines,
            true,
            false,
            PLAN_TTL,
            ttl,
        )
    }

    /// Ask for a STEP approval, or the Submit variant when `submit` is set. `changed` marks a
    /// drift pause. `ttl` can only tighten [`STEP_TTL`].
    #[allow(clippy::too_many_arguments)]
    pub fn request_step(
        &self,
        digest: &ApprovalDigest,
        step_index: usize,
        step: &StepFacts,
        submit: bool,
        live: bool,
        changed: bool,
        ttl: Option<Duration>,
    ) -> Result<PendingApproval, RequestError> {
        let l = summary::line(step_index, step);
        let summary = summary::step_summary(&l, submit, changed, digest.short());
        let kind = if submit {
            ApprovalKindDto::Submit
        } else {
            ApprovalKindDto::Step
        };
        self.submit(
            kind,
            digest.hex().to_string(),
            summary,
            vec![l],
            live,
            changed,
            STEP_TTL,
            ttl,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn submit(
        &self,
        kind: ApprovalKindDto,
        digest_hex: String,
        summary: String,
        lines: Vec<ApprovalLineDto>,
        live: bool,
        changed: bool,
        max_ttl: Duration,
        ttl: Option<Duration>,
    ) -> Result<PendingApproval, RequestError> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|_| RequestError::NoRandomness)?;
        let id = hex(&raw);
        let ttl = ttl.map_or(max_ttl, |t| t.min(max_ttl));
        let dto = ApprovalRequestDto {
            id: id.clone(),
            kind,
            digest_hex,
            summary,
            lines,
            live,
            changed,
            timeout_secs: u32::try_from(ttl.as_secs()).unwrap_or(u32::MAX),
        };
        {
            let mut st = lock(&self.inner.state);
            if st.pending.len() >= MAX_PENDING {
                return Err(RequestError::TooMany);
            }
            let seq = st.next_seq;
            st.next_seq += 1;
            st.pending.insert(
                id.clone(),
                Entry {
                    dto: dto.clone(),
                    created_seq: seq,
                    expires: self.inner.clock.now().saturating_add(ttl),
                    displayed: None,
                    epoch: self.inner.kill.epoch(),
                },
            );
        }
        // Outside the lock: a slow window never blocks a response or a kill.
        if !self.ui().show(&dto) {
            self.resolve(&id, ApprovalOutcome::Denied);
        }
        Ok(PendingApproval {
            broker: self.clone(),
            id,
            done: false,
        })
    }

    /// The window shows this request now: starts the 500 ms guard. Repeating it (a reloaded
    /// window) restarts the guard, never shortens it.
    pub fn mark_displayed(&self, id: &str) -> Result<(), RespondError> {
        let mut st = lock(&self.inner.state);
        let now = self.inner.clock.now();
        match st.pending.get_mut(id) {
            Some(e) => {
                e.displayed = Some(e.displayed.map_or(now, |d| d.max(now)));
                Ok(())
            }
            None => Err(RespondError::UnknownId),
        }
    }

    /// The window's answer. See the module docs for what is refused.
    pub fn respond(
        &self,
        id: &str,
        decision: ApprovalDecisionDto,
        echoed_digest: &str,
    ) -> Result<(), RespondError> {
        let mut closed: Closed = Vec::new();
        let result = {
            let mut st = lock(&self.inner.state);
            let now = self.inner.clock.now();
            let r = self.check_and_take(&mut st, id, decision, echoed_digest, now, &mut closed);
            if !closed.is_empty() {
                for (cid, o) in &closed {
                    st.resolved.insert(cid.clone(), *o);
                }
            }
            r
        };
        self.finish(closed);
        result
    }

    fn check_and_take(
        &self,
        st: &mut State,
        id: &str,
        decision: ApprovalDecisionDto,
        echoed: &str,
        now: Duration,
        closed: &mut Closed,
    ) -> Result<(), RespondError> {
        let Some(e) = st.pending.get(id) else {
            return Err(RespondError::UnknownId);
        };
        if self.inner.kill.is_tripped_since(e.epoch) {
            st.pending.remove(id);
            closed.push((id.to_string(), ApprovalOutcome::Killed));
            return Err(RespondError::Killed);
        }
        if now >= e.expires {
            st.pending.remove(id);
            closed.push((id.to_string(), ApprovalOutcome::Expired));
            return Err(RespondError::Expired);
        }
        if !ct_eq(e.dto.digest_hex.as_bytes(), echoed.as_bytes()) {
            return Err(RespondError::DigestMismatch);
        }
        let outcome = match decision {
            ApprovalDecisionDto::Deny => ApprovalOutcome::Denied,
            ApprovalDecisionDto::Approve => {
                let Some(shown) = e.displayed else {
                    return Err(RespondError::NotDisplayed);
                };
                if now.saturating_sub(shown) < INPUT_GUARD {
                    return Err(RespondError::TooEarly);
                }
                ApprovalOutcome::Approved
            }
        };
        st.pending.remove(id);
        closed.push((id.to_string(), outcome));
        Ok(())
    }

    /// Resolve one request (used for UI-unavailable and cancellation).
    fn resolve(&self, id: &str, outcome: ApprovalOutcome) {
        let mut closed: Closed = Vec::new();
        {
            let mut st = lock(&self.inner.state);
            if st.pending.remove(id).is_some() {
                st.resolved.insert(id.to_string(), outcome);
                closed.push((id.to_string(), outcome));
            }
        }
        self.finish(closed);
    }

    /// Tell the window and wake the waiters. Never called with the state lock held.
    fn finish(&self, closed: Closed) {
        if closed.is_empty() {
            return;
        }
        self.inner.cv.notify_all();
        let ui = self.ui();
        for (id, o) in closed {
            ui.close(&ApprovalClosedDto {
                id,
                outcome: o.dto(),
            });
        }
    }

    /// Resolve every request past its deadline `Expired`. Returns how many. Waiting callers do
    /// this by themselves; the window shell may call it on a timer too.
    pub fn expire_due(&self) -> usize {
        let mut closed: Closed = Vec::new();
        {
            let mut st = lock(&self.inner.state);
            let now = self.inner.clock.now();
            let due: Vec<String> = st
                .pending
                .iter()
                .filter(|(_, e)| now >= e.expires)
                .map(|(k, _)| k.clone())
                .collect();
            for id in due {
                st.pending.remove(&id);
                st.resolved.insert(id.clone(), ApprovalOutcome::Expired);
                closed.push((id, ApprovalOutcome::Expired));
            }
        }
        let n = closed.len();
        self.finish(closed);
        n
    }

    /// Every pending request resolves `Killed` and the window is told to close. Called by the
    /// kill switch hook; safe to call any number of times.
    pub fn kill_all(&self) {
        self.resolve_all(ApprovalOutcome::Killed);
    }

    /// Every pending request is Denied (the window was closed by the user).
    pub fn deny_all(&self) {
        self.resolve_all(ApprovalOutcome::Denied);
    }

    fn resolve_all(&self, outcome: ApprovalOutcome) {
        let mut closed: Closed = Vec::new();
        {
            let mut st = lock(&self.inner.state);
            let ids: Vec<String> = st.pending.keys().cloned().collect();
            for id in ids {
                st.pending.remove(&id);
                st.resolved.insert(id.clone(), outcome);
                closed.push((id, outcome));
            }
        }
        self.finish(closed);
    }

    /// The requests still waiting, oldest first (a window that mounts late pulls these).
    pub fn pending_requests(&self) -> Vec<ApprovalRequestDto> {
        let st = lock(&self.inner.state);
        let mut v: Vec<(u64, ApprovalRequestDto)> = st
            .pending
            .values()
            .map(|e| (e.created_seq, e.dto.clone()))
            .collect();
        v.sort_by_key(|(s, _)| *s);
        v.into_iter().map(|(_, d)| d).collect()
    }

    pub fn pending_count(&self) -> usize {
        lock(&self.inner.state).pending.len()
    }
}

/// A request that was shown. `wait` blocks (run it on a blocking thread, never in async code)
/// until the human answers, the deadline passes or the kill switch fires. Dropping it without
/// an answer cancels the request (Denied) and closes the window entry.
pub struct PendingApproval {
    broker: ApprovalBroker,
    id: String,
    done: bool,
}

impl PendingApproval {
    /// The id the window answers with (also in the DTO it was shown with).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The outcome if the request already ended.
    pub fn try_outcome(&self) -> Option<ApprovalOutcome> {
        lock(&self.broker.inner.state)
            .resolved
            .get(&self.id)
            .copied()
    }

    /// Block until the request ends. With a manual clock this re-reads the clock every
    /// millisecond of real time.
    pub fn wait(mut self) -> ApprovalOutcome {
        let inner = Arc::clone(&self.broker.inner);
        loop {
            self.broker.expire_due();
            let mut st = lock(&inner.state);
            if let Some(o) = st.resolved.remove(&self.id) {
                self.done = true;
                return o;
            }
            if !st.pending.contains_key(&self.id) {
                // Gone without a result (cannot happen): fail closed.
                self.done = true;
                return ApprovalOutcome::Denied;
            }
            let now = inner.clock.now();
            let remaining = st
                .pending
                .get(&self.id)
                .map_or(Duration::ZERO, |e| e.expires.saturating_sub(now));
            let slice = inner.clock.poll_slice(remaining);
            let _ = inner.cv.wait_timeout(st, slice);
        }
    }
}

impl Drop for PendingApproval {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // Cancel: no longer pending, no stored result left behind, window told to close.
        let was_pending = lock(&self.broker.inner.state)
            .pending
            .contains_key(&self.id);
        if was_pending {
            self.broker.resolve(&self.id, ApprovalOutcome::Denied);
        }
        lock(&self.broker.inner.state).resolved.remove(&self.id);
    }
}

impl std::fmt::Debug for PendingApproval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingApproval").finish_non_exhaustive()
    }
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

/// Length-aware comparison without an early exit on the first differing byte.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests;
