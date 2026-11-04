//! Deterministic seeded model-based test (S2-11b criterion 9): random sequences of API calls
//! (propose / dry run / approve / decide / wait / authorize / void / kill / clock moves) against a
//! gate with a manual clock. Invariants, checked on every call:
//! - never two `ApprovedAction`s for the same (plan digest, step);
//! - never an `ApprovedAction` while the mode is Observe;
//! - never an `ApprovedAction` from a plan grant issued before the last `void_all` / kill;
//! - every `ApprovedAction` minted before a `void_all` / kill is dead (`check_live` fails);
//! - exactly one intent record per mint (and none for a refusal).

mod common;

use std::collections::HashSet;

use common::world::*;
use firewall::{Observation, SessionState};
use policy::*;

const SEQUENCES: u64 = 2500;
const OPS_PER_SEQUENCE: usize = 60;

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn plans() -> Vec<String> {
    vec![
        steps_json(&[
            OBSERVE,
            &click("Notes"),
            &type_into("Name", "John Smith"),
            &click("Refresh"),
            &click("Save"),
        ]),
        steps_json(&[&click("Notes"), SCROLL, &type_into("Name", "John Smith")]),
        steps_json(&[&click("Delete"), &click("Notes"), &type_into("Name", "xyz")]),
    ]
}

struct Pending {
    approval: PendingPlanApproval,
    decided: bool,
    deadline_ms: u64,
}

struct Run<'a> {
    rig: Rig,
    pool: &'a [Observation],
    rng: Rng,
    plans: Vec<String>,
    proposal: Option<ProposedPlan>,
    report: Option<DryRunReport>,
    pending: Option<Pending>,
    grants: Vec<PlanGrant>,
    /// Digests of plan grants issued since the last void/kill.
    valid: HashSet<String>,
    minted_keys: HashSet<(String, u16)>,
    since_void: Vec<ApprovedAction>,
    outstanding: Option<ActionDigest>,
    now_ms: u64,
    mint_count: usize,
}

impl<'a> Run<'a> {
    fn new(seed: u64, pool: &'a [Observation]) -> Self {
        let rig = Rig::new();
        Run {
            rig,
            pool,
            rng: Rng::new(seed),
            plans: plans(),
            proposal: None,
            report: None,
            pending: None,
            grants: Vec::new(),
            valid: HashSet::new(),
            minted_keys: HashSet::new(),
            since_void: Vec::new(),
            outstanding: None,
            now_ms: 0,
            mint_count: 0,
        }
    }

    fn advance(&mut self, ms: u64) {
        self.now_ms += ms;
        self.rig.advance(ms);
    }

    fn voided(&mut self) {
        self.valid.clear();
        for a in &self.since_void {
            assert!(a.check_live().is_err(), "action survived a void/kill");
        }
        self.since_void.clear();
        self.outstanding = None;
        self.pending = self.pending.take().map(|mut p| {
            p.decided = true; // a voided approval returns immediately
            p
        });
    }

    fn op(&mut self) {
        let k = self.rng.below(17);
        self.op_k(k);
    }

    fn op_k(&mut self, k: u64) {
        match k {
            0 => {
                let i = self.rng.below(self.plans.len() as u64) as usize;
                let cat = catalogue();
                let task = TaskId(1 + self.rng.below(3));
                if let Ok(p) =
                    self.rig
                        .gate
                        .propose(task, &self.plans[i], &PlanContext::new(TRANSCRIPT, &cat))
                {
                    self.proposal = Some(p);
                    self.report = None;
                    self.pending = None; // superseded task voids its approvals
                    self.outstanding = None;
                }
            }
            1 | 2 => {
                if let Some(p) = &self.proposal {
                    let obs = &self.pool[self.rng.below(self.pool.len() as u64) as usize];
                    let ctx = self.rig.ctx(obs, 0);
                    self.report = Some(self.rig.gate.dry_run(p, obs, &ctx));
                }
            }
            3 | 4 => {
                if let (Some(p), Some(r), None) = (&self.proposal, &self.report, &self.pending) {
                    if let Ok(a) = self.rig.gate.request_plan_approval(p, r) {
                        self.pending = Some(Pending {
                            approval: a,
                            decided: false,
                            deadline_ms: self.now_ms + 120_000,
                        });
                    }
                }
            }
            5 | 6 => {
                if self.pending.is_some() && self.rng.chance(85) {
                    self.advance(600);
                }
                if let Some(p) = &mut self.pending {
                    let wrong = self.rng.chance(15);
                    let d = if self.rng.chance(80) {
                        Decision::Approve
                    } else {
                        Decision::Deny
                    };
                    let hex = if wrong {
                        "ab".repeat(32)
                    } else {
                        p.approval.digest_hex()
                    };
                    if self.rig.gate.decide(p.approval.id(), &hex, d).is_ok() {
                        p.decided = true;
                    }
                }
            }
            7 => {
                // Wait only when it cannot block: decided, or past the deadline.
                let ready = self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.decided || self.now_ms >= p.deadline_ms);
                if ready {
                    let p = self.pending.take().unwrap();
                    if let Ok(g) = p.approval.wait() {
                        // A live grant only exists for a plan approved in this epoch.
                        self.valid.insert(g.plan_digest().hex());
                        self.grants.push(g);
                    }
                }
            }
            8..=11 => self.run_step(),
            15 => {
                if let Some(d) = self.outstanding.take() {
                    let r = if self.rng.chance(85) {
                        ExecResult::executed()
                    } else {
                        ExecResult::failed("driver_failed")
                    };
                    self.rig.gate.record_result(d, &r);
                }
            }
            12 => {
                if !self.rng.chance(25) {
                    return;
                }
                if self.rng.chance(50) {
                    self.rig.gate.void_all(VoidReason::SettingsChanged);
                } else {
                    self.rig.gate.set_observe();
                }
                self.voided();
            }
            13 => {
                if self.rng.chance(20) {
                    self.rig.kill.trip(KillReason::Hotkey);
                    self.voided();
                }
            }
            14 => {
                let ms =
                    [0, 100, 300, 300, 700, 5_000, 31_000, 130_000][self.rng.below(8) as usize];
                self.advance(ms);
            }
            _ => {
                if self.rng.chance(10) {
                    self.pending = None; // drop = cancel
                }
            }
        }
    }

    fn run_step(&mut self) {
        if self.grants.is_empty() {
            return;
        }
        // Mostly the newest grant, sometimes an old one (must never work after void/new plan).
        let gi = if self.rng.chance(75) {
            self.grants.len() - 1
        } else {
            self.rng.below(self.grants.len() as u64) as usize
        };
        let Some(plan) = &self.proposal else { return };
        let idx = {
            let n = self.grants[gi].next_step() as usize;
            if self.rng.chance(15) {
                self.rng.below(plan.len() as u64) as usize
            } else {
                n
            }
        };
        if idx >= plan.len() {
            return;
        }
        let obs = &self.pool[self.rng.below(self.pool.len() as u64) as usize];
        if self
            .rig
            .gate
            .note_observation(obs.id(), SessionState::Active)
            .is_err()
        {
            return;
        }
        let Ok(step) = bind_step(&plan.steps()[idx], obs, &self.rig.ctx(obs, idx as u16)) else {
            return;
        };
        if step.action().is_none() {
            let _ = self.rig.gate.complete_read(&step, &mut self.grants[gi]);
            return;
        }
        // Step approval, when asked and when it cannot block.
        let mut single = None;
        if self.rng.chance(60)
            && matches!(
                self.rig.gate.evaluate(&step, self.grants.get(gi)),
                Evaluation::NeedsApproval
            )
        {
            if let Ok(p) = self.rig.gate.request_step_approval(&step) {
                let ms = if self.rng.chance(90) { 600 } else { 100 };
                self.now_ms += ms;
                self.rig.advance(ms);
                let ok = self
                    .rig
                    .gate
                    .decide(p.id(), &p.digest_hex(), Decision::Approve)
                    .is_ok();
                if ok {
                    single = p.wait().ok();
                }
            }
        }
        let mode_before = self.rig.gate.mode();
        let digest = self.grants[gi].plan_digest().hex();
        let key = (digest.clone(), step.index());
        let intents_before = self.rig.audit.intents().len();
        let result = self.rig.gate.authorize(step, &mut self.grants[gi], single);
        if let Ok(a) = result {
            assert_eq!(mode_before, Mode::Live, "minted in Observe mode");
            assert!(
                self.valid.contains(&digest),
                "minted without a live plan grant"
            );
            assert!(
                self.minted_keys.insert(key),
                "same (plan, step) minted twice"
            );
            assert_eq!(
                self.rig.audit.intents().len(),
                intents_before + 1,
                "one intent per mint"
            );
            self.mint_count += 1;
            assert!(a.check_live().is_ok());
            let d = a.digest();
            self.since_void.push(a);
            if self.rng.chance(80) {
                self.rig.gate.record_result(d, &ExecResult::executed());
                self.outstanding = None;
            } else {
                self.outstanding = Some(d);
            }
        } else {
            assert_eq!(self.rig.audit.intents().len(), intents_before);
        }
        let ms = self.rng.below(400);
        self.now_ms += ms;
        self.rig.advance(ms);
    }
}

#[test]
fn model_based_sequences_never_break_the_invariants() {
    let desk = desktop();
    let pool: Vec<Observation> = (0..40).map(|_| observe_of(&desk)).collect();
    let mut total_mints = 0usize;
    for seed in 0..SEQUENCES {
        let mut run = Run::new(seed, &pool);
        // A mostly-happy prefix (each step dropped 15 % of the time) so the sequences reach the
        // interesting states; then pure chaos.
        for k in [0, 1, 3, 5, 7, 8, 8, 8, 8, 8] {
            if !run.rng.chance(15) {
                run.op_k(k);
            }
        }
        for _ in 0..OPS_PER_SEQUENCE {
            run.op();
        }
        total_mints += run.mint_count;
    }
    // The model is not vacuous: it did mint a good number of actions along the way.
    assert!(total_mints > 500, "only {total_mints} mints");
}
