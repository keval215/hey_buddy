// Typed wrappers over the Tauri IPC (commands and events from `buddy-core::api`).
import { invoke } from "@tauri-apps/api/core";
import { type UnlistenFn, listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { Decision } from "../ipc/Decision";
import type { LastRequestDto } from "../ipc/LastRequestDto";
import type { PreviewClosed } from "../ipc/PreviewClosed";
import type { PreviewDto } from "../ipc/PreviewDto";
import type { EngineSwapEvent } from "../ipc/EngineSwapEvent";
import type { SwapTarget } from "../ipc/SwapTarget";
import type { DisplayDto } from "../ipc/DisplayDto";
import type { FirewallStatus } from "../ipc/FirewallStatus";
import type { PointEvent } from "../ipc/PointEvent";
import type { Settings } from "../ipc/Settings";
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
  engineSwapRequested: "engine_swap_requested",
  engineSwapDone: "engine_swap_done",
  engineSwapFailed: "engine_swap_failed",
} as const;

export interface EventMap {
  "turn:state": TurnState;
  "turn:point": PointEvent;
  "firewall:status": FirewallStatus;
  "turn:error": TurnError;
  "turn:preview": PreviewDto;
  "turn:preview_closed": PreviewClosed;
  engine_swap_requested: EngineSwapEvent;
  engine_swap_done: EngineSwapEvent;
  engine_swap_failed: EngineSwapEvent;
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
  updateSettings: (patch: SettingsPatch) => invoke<Settings>("update_settings", { patch }),
  /** Settings window only. Loads the current profile's voice engines in the background; progress arrives as engine_swap_* events. */
  requestEngineSwap: (slot: SwapTarget = "all") => invoke<void>("request_engine_swap", { slot }),
  listDisplays: () => invoke<DisplayDto[]>("list_displays"),
  cancelTurn: () => invoke<void>("cancel_turn"),
  /** Preview window only. */
  previewDecision: (requestId: number, decision: Decision) =>
    invoke<void>("preview_decision", { requestId, decision }),
  /** Preview window only: the preview waiting for a decision (a panel that mounts late). */
  getPendingPreview: () => invoke<PreviewDto | null>("get_pending_preview"),
  getLastRequest: () => invoke<LastRequestDto | null>("get_last_request"),
  /** The key goes to the OS keychain; it is never returned. Provider allowlist: "anthropic". */
  setApiKey: (provider: string, key: string) => invoke<Settings>("set_api_key", { provider, key }),
  clearApiKey: (provider: string) => invoke<Settings>("clear_api_key", { provider }),
  /** Dev builds only; the command does not exist in release. */
  debugPoint: (x: number, y: number, label: string) =>
    invoke<void>("debug_point", { x, y, label }),
};
