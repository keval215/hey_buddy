// Typed wrappers over the Tauri IPC (commands and events from `buddy-core::api`).
import { invoke } from "@tauri-apps/api/core";
import { type UnlistenFn, listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { AgentStatusDto } from "../ipc/AgentStatusDto";
import type { AgentTaskDto } from "../ipc/AgentTaskDto";
import type { ApprovalClosedDto } from "../ipc/ApprovalClosedDto";
import type { ApprovalDecisionDto } from "../ipc/ApprovalDecisionDto";
import type { ApprovalRequestDto } from "../ipc/ApprovalRequestDto";
import type { Decision } from "../ipc/Decision";
import type { LastRequestDto } from "../ipc/LastRequestDto";
import type { PreviewClosed } from "../ipc/PreviewClosed";
import type { PreviewDto } from "../ipc/PreviewDto";
import type { DownloadDone } from "../ipc/DownloadDone";
import type { DownloadFailed } from "../ipc/DownloadFailed";
import type { DownloadProgress } from "../ipc/DownloadProgress";
import type { ModelRow } from "../ipc/ModelRow";
import type { EngineSwapEvent } from "../ipc/EngineSwapEvent";
import type { SwapTarget } from "../ipc/SwapTarget";
import type { DisplayDto } from "../ipc/DisplayDto";
import type { FirewallStatus } from "../ipc/FirewallStatus";
import type { PointEvent } from "../ipc/PointEvent";
import type { AuditClearDto } from "../ipc/AuditClearDto";
import type { AuditExportDto } from "../ipc/AuditExportDto";
import type { AuditPageDto } from "../ipc/AuditPageDto";
import type { DenyListDto } from "../ipc/DenyListDto";
import type { Settings } from "../ipc/Settings";
import type { SettingsResult } from "../ipc/SettingsResult";
import type { SettingsPatch } from "../ipc/SettingsPatch";
import type { Status } from "../ipc/Status";
import type { TurnError } from "../ipc/TurnError";
import type { TurnState } from "../ipc/TurnState";

export const EVT = {
  turnState: "turn:state",
  turnPoint: "turn:point",
  firewallStatus: "firewall:status",
  turnError: "turn:error",
  turnPreview: "turn:preview",
  previewClosed: "turn:preview_closed",
  approvalRequest: "approval:request",
  approvalClosed: "approval:closed",
  engineSwapRequested: "engine_swap_requested",
  engineSwapDone: "engine_swap_done",
  engineSwapFailed: "engine_swap_failed",
  downloadProgress: "download_progress",
  downloadDone: "download_done",
  downloadFailed: "download_failed",
  agentTaskDone: "agent:task_done",
} as const;

export interface EventMap {
  "turn:state": TurnState;
  "turn:point": PointEvent;
  "firewall:status": FirewallStatus;
  "turn:error": TurnError;
  "turn:preview": PreviewDto;
  "turn:preview_closed": PreviewClosed;
  "approval:request": ApprovalRequestDto;
  "approval:closed": ApprovalClosedDto;
  engine_swap_requested: EngineSwapEvent;
  engine_swap_done: EngineSwapEvent;
  engine_swap_failed: EngineSwapEvent;
  download_progress: DownloadProgress;
  download_done: DownloadDone;
  download_failed: DownloadFailed;
  "agent:task_done": AgentTaskDto;
}

export function on<K extends keyof EventMap>(
  name: K,
  handler: (payload: EventMap[K]) => void,
): Promise<UnlistenFn> {
  return listen<EventMap[K]>(name, (e) => handler(e.payload));
}

/**
 * Listen on THIS window only. The preview events are emitted to the `preview` window; a
 * window-scoped listener is the narrowest way to receive them.
 */
export function onThisWindow<K extends keyof EventMap>(
  name: K,
  handler: (payload: EventMap[K]) => void,
): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().listen<EventMap[K]>(name, (e) => handler(e.payload));
}

export const api = {
  getStatus: () => invoke<Status>("get_status"),
  getSettings: () => invoke<Settings>("get_settings"),
  /** Invalid values fall back to the safest value and come back as `issues`. */
  updateSettings: (patch: SettingsPatch) => invoke<SettingsResult>("update_settings", { patch }),
  /** Settings window only. Deny-list editor; every change applies from the next turn. */
  getDenyList: () => invoke<DenyListDto>("get_deny_list"),
  addDenyEntry: (id: string) => invoke<DenyListDto>("add_deny_entry", { id }),
  /** A built-in entry is only removed with `confirmRemoveDefault` (after the UI's second step). */
  removeDenyEntry: (id: string, confirmRemoveDefault = false) =>
    invoke<DenyListDto>("remove_deny_entry", { id, confirmRemoveDefault }),
  resetDenyListToDefaults: (confirm: boolean) =>
    invoke<DenyListDto>("reset_deny_list_to_defaults", { confirm }),
  /** Settings window only. The agent activity log, newest first; content-free. */
  getAuditEntries: (cursor: number, limit: number) =>
    invoke<AuditPageDto>("get_audit_entries", { cursor, limit }),
  /** Explicit click: copy the log to a NEW file at `destPath` (never overwrites). */
  exportAuditLog: (destPath: string) => invoke<AuditExportDto>("export_audit_log", { destPath }),
  /** Explicit click after a second confirm; the core refuses without `confirm: true`. */
  clearAuditLog: (confirm: boolean) => invoke<AuditClearDto>("clear_audit_log", { confirm }),
  /** Settings window only. Loads the current profile's voice engines in the background; progress arrives as engine_swap_* events. */
  requestEngineSwap: (slot: SwapTarget = "all") => invoke<void>("request_engine_swap", { slot }),
  /** Settings window only. The models table; reads the local filesystem, never the network. */
  listModels: () => invoke<ModelRow[]>("list_models"),
  /**
   * Settings window only, explicit click. `acceptUnknownChecksum` is only sent after the second
   * confirm for models whose checksum is unknown. Progress arrives as download_* events.
   */
  startDownload: (id: string, acceptUnknownChecksum = false) =>
    invoke<void>("start_download", { id, acceptUnknownChecksum }),
  cancelDownload: (id: string) => invoke<void>("cancel_download", { id }),
  listDisplays: () => invoke<DisplayDto[]>("list_displays"),
  cancelTurn: () => invoke<void>("cancel_turn"),
  /** STOP everything (S2-13). Safe from any window; never blocks. */
  killAgent: () => invoke<void>("kill_agent"),
  /** Preview window only. */
  previewDecision: (requestId: number, decision: Decision) =>
    invoke<void>("preview_decision", { requestId, decision }),
  /** Preview window only: the preview waiting for a decision (a panel that mounts late). */
  getPendingPreview: () => invoke<PreviewDto | null>("get_pending_preview"),
  /** Approval window only: approvals waiting now (a window that mounts late). */
  approvalReady: () => invoke<ApprovalRequestDto[]>("approval_ready"),
  /** Approval window only: this request is on screen (starts the 500 ms input guard). */
  approvalDisplayed: (id: string) => invoke<void>("approval_displayed", { id }),
  /** Approval window only: answer a request core created; the digest must be echoed back. */
  approvalRespond: (id: string, decision: ApprovalDecisionDto, echoedDigest: string) =>
    invoke<void>("approval_respond", { id, decision, echoedDigest }),
  getLastRequest: () => invoke<LastRequestDto | null>("get_last_request"),
  /** The key goes to the OS keychain; it is never returned. Provider allowlist: "anthropic". */
  setApiKey: (provider: string, key: string) => invoke<Settings>("set_api_key", { provider, key }),
  clearApiKey: (provider: string) => invoke<Settings>("clear_api_key", { provider }),
  /** Settings window only. Whether the agent can start a task now, and why not (fixed tokens). */
  agentStatus: () => invoke<AgentStatusDto>("agent_status"),
  /**
   * Settings window only. Starts one agent task and returns at once; the outcome arrives as
   * `agent:task_done`. Rejected with a fixed token (`agent_disabled`, `busy`, ...). Every step
   * still needs its approvals.
   */
  agentStartTask: (transcript: string) => invoke<void>("agent_start_task", { transcript }),
  /** Dev builds only; the command does not exist in release. */
  debugPoint: (x: number, y: number, label: string) =>
    invoke<void>("debug_point", { x, y, label }),
};
