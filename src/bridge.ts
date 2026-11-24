import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  desktopCommands,
  sharedEvents,
  type CommandName,
  type CommandParams,
  type CommandResult,
  type EventMap,
  type RemoteEvent,
} from "./bridge-contracts";
import { currentInstance, setInstanceState } from "./instance";
export const native = "__TAURI_INTERNALS__" in window;
export const preview = import.meta.env.DEV && !native;
const desktop = new Set<string>(desktopCommands);
const shared = new Set<string>(sharedEvents);
/**
 * Desktop commands run in this app. Shared ones run on the current instance:
 * `call` for this machine, `remote_call` for a server.
 */
export async function command<K extends CommandName>(
  name: K,
  ...params: CommandParams<K>
): Promise<CommandResult<K>> {
  const args = params[0] ?? {};
  if (native) {
    if (desktop.has(name)) return invoke<CommandResult<K>>(name, args);
    await ready;
    const instance = currentInstance();
    return instance === "local"
      ? invoke<CommandResult<K>>("call", { name, args })
      : invoke<CommandResult<K>>("remote_call", { instance, name, args });
  }
  // The preview is one in-memory instance whichever one is selected.
  if (preview)
    return (await import("./preview")).run(name, args) as Promise<
      CommandResult<K>
    >;
  throw new Error("请在 Vela 桌面客户端中打开");
}
/**
 * Shared events arrive from this machine under their own name and from servers
 * as `remote-event`; only the current instance's reach `fn`.
 */
export async function subscribe<K extends keyof EventMap>(
  event: K,
  fn: (payload: EventMap[K]) => void,
): Promise<() => void> {
  if (native) {
    if (!shared.has(event))
      return listen<EventMap[K]>(event, (e) => fn(e.payload));
    const offLocal = await listen<EventMap[K]>(event, (e) => {
      if (currentInstance() === "local") fn(e.payload);
    });
    try {
      const offRemote = await listen<RemoteEvent>("remote-event", (e) => {
        if (e.payload.instance === currentInstance() && e.payload.event === event)
          fn(e.payload.payload as EventMap[K]);
      });
      return () => {
        offLocal();
        offRemote();
      };
    } catch (error) {
      offLocal();
      throw error;
    }
  }
  if (preview) return (await import("./preview")).subscribe(event, fn);
  return () => {};
}
/**
 * Settles once the current instance is known, so the first shared command
 * already goes to the right place. A failed read leaves this machine selected.
 */
export const ready: Promise<void> = (async () => {
  if (!native && !preview) return;
  let received = false;
  try {
    await subscribe("instance-state", (state) => {
      received = true;
      setInstanceState(state);
    });
    const state = await command("get_instances");
    if (!received) setInstanceState(state);
  } catch {
    // The manage dialog reports the failure when it reads the list itself.
  }
})();
