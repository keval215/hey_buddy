//! How the core reports to the outside world. The Tauri glue implements [`EventSink`] and turns
//! each call into an event; tests record them. Payloads are the fixed IPC types of
//! [`crate::api`]: only `transcript` and `reply_delta` carry content, and that content is the
//! intended payload of exactly those two events.

use crate::api::{
    EngineSwapEvent, FirewallStatus, PointEvent, ReplyDelta, TranscriptEvent, TurnError, TurnState,
};

pub trait EventSink: Send + Sync {
    /// `turn:state`.
    fn turn_state(&self, ev: TurnState);
    /// `turn:transcript`.
    fn transcript(&self, ev: TranscriptEvent);
    /// `turn:reply_delta` (tags already removed).
    fn reply_delta(&self, ev: ReplyDelta);
    /// `turn:point`.
    fn point(&self, ev: PointEvent);
    /// `firewall:status`.
    fn firewall_status(&self, st: FirewallStatus);
    /// `turn:error`.
    fn error(&self, e: TurnError);
    /// `engine_swap_requested`. Default: dropped.
    fn engine_swap_requested(&self, _ev: EngineSwapEvent) {}
    /// `engine_swap_done`. Default: dropped.
    fn engine_swap_done(&self, _ev: EngineSwapEvent) {}
    /// `engine_swap_failed`. Default: dropped.
    fn engine_swap_failed(&self, _ev: EngineSwapEvent) {}
}

/// Drops every event (the default until the glue installs a sink).
pub struct NullSink;

impl EventSink for NullSink {
    fn turn_state(&self, _: TurnState) {}
    fn transcript(&self, _: TranscriptEvent) {}
    fn reply_delta(&self, _: ReplyDelta) {}
    fn point(&self, _: PointEvent) {}
    fn firewall_status(&self, _: FirewallStatus) {}
    fn error(&self, _: TurnError) {}
}
