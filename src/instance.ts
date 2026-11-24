import { useSyncExternalStore } from "react";
import type { InstanceState, SharedCommands } from "./bridge-contracts";

// Which Vela the pages show and operate: this machine or a server reached over
// SSH. `bridge.ts` keeps it current and routes by it; components read it here.
let snapshot: { state: InstanceState; epoch: number } = {
  state: {
    revision: "",
    active: "local",
    machines: [],
    connection: { status: "local", version: null, error: null, unsupported: [] },
  },
  epoch: 0,
};
const listeners = new Set<() => void>();
const watch = (fn: () => void) => {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
};
export const instanceState = () => snapshot.state;
export const currentInstance = () => snapshot.state.active;
export function setInstanceState(state: InstanceState) {
  const before = snapshot.state;
  // Pages hold what they read from the instance, so they start over when the
  // instance changes and when a remote one comes back.
  const fresh =
    state.active !== before.active ||
    (state.connection.status === "connected" &&
      before.connection.status !== "connected");
  snapshot = { state, epoch: snapshot.epoch + (fresh ? 1 : 0) };
  for (const fn of listeners) fn();
}
export function useInstance() {
  const { state, epoch } = useSyncExternalStore(watch, () => snapshot);
  const remote = state.active !== "local";
  return {
    state,
    /** The pages act on a server: entries that only work on this machine go away. */
    remote,
    /** Changes whenever the pages have to read everything again. */
    key: `${state.active}#${epoch}`,
    supports: (name: keyof SharedCommands) =>
      !remote || !state.connection.unsupported.includes(name),
  };
}
