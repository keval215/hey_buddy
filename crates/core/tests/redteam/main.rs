//! The red-team suite (S2-22). Every test is one named attack with a doc line
//! `Attack / Expected / Defence`; `docs/redteam.md` lists them all. Linux, fakes only, no network,
//! public API only (the `fakes` features core already uses as dev-dependencies). Each case asserts
//! that NOTHING executed (the `FakeActuator` call log and the write-ahead intents stay empty,
//! unless the case says what ran) and a typed refusal / outcome token.
//!
//! Run: `cargo test -p buddy-core --test redteam`. CI also checks the suite is not vacuous
//! (`tools/check-redteam.sh`).

#[path = "../../../policy/tests/common/world.rs"]
mod world;

mod rig;

mod a_injection;
mod b_titles;
mod c_feedback;
mod d_secure;
mod e_deny;
mod f_payment;
mod g_drift;
mod h_hostile_engine;
mod i_approval;
mod j_delimiters;
mod k_huge;
mod l_never;
mod m_kill;
mod n_origin;
mod o_audit;
mod p_settings;
mod q_leaks;
mod z_gate;
