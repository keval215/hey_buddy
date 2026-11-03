// Typed wrappers over the Tauri IPC (commands and events from `buddy-core::api`).
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
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
} as const;

export interface EventMap {
  "turn:state": TurnState;
  "turn:point": PointEvent;
  "firewall:status": FirewallStatus;
  "turn:error": TurnError;
}

export function on<K extends keyof EventMap>(
  name: K,
  handler: (payload: EventMap[K]) => void,
): Promise<UnlistenFn> {
  return listen<EventMap[K]>(name, (e) => handler(e.payload));
}

export const api = {
  getStatus: () => invoke<Status>("get_status"),
  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (patch: SettingsPatch) => invoke<Settings>("update_settings", { patch }),
  listDisplays: () => invoke<DisplayDto[]>("list_displays"),
  cancelTurn: () => invoke<void>("cancel_turn"),
  /** Dev builds only; the command does not exist in release. */
  debugPoint: (x: number, y: number, label: string) =>
    invoke<void>("debug_point", { x, y, label }),
};
