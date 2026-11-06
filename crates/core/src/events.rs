//! How the core reports to the outside world. The Tauri glue implements [`EventSink`] and turns
//! each call into an event; tests record them. Payloads are the fixed IPC types of
//! [`crate::api`]: only `transcript` and `reply_delta` carry content, and that content is the
//! intended payload of exactly those two events.

use crate::api::{
    DownloadDone, DownloadFailed, DownloadProgress, EngineSwapEvent, FirewallStatus, PointEvent,
    ReplyDelta, TranscriptEvent, TurnError, TurnState,
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
    /// `download_progress` (throttled). Default: dropped.
    fn download_progress(&self, _ev: DownloadProgress) {}
    /// `download_done`. Default: dropped.
    fn download_done(&self, _ev: DownloadDone) {}
    /// `download_failed`. Default: dropped.
    fn download_failed(&self, _ev: DownloadFailed) {}
    /// `update_progress` (throttled; ids are `update`). Default: dropped.
    fn update_progress(&self, _ev: DownloadProgress) {}
    /// `update_done`. Default: dropped.
    fn update_done(&self, _ev: DownloadDone) {}
    /// `update_failed`. Default: dropped.
    fn update_failed(&self, _ev: DownloadFailed) {}
    /// `agent:task_done` (S2-23b): how an agent task ended, as fixed tokens and counts, plus the
    /// short read answers. Default: dropped.
    fn agent_task(&self, _ev: crate::agent::AgentTaskDto) {}
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
