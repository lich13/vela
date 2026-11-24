import { beforeEach, expect, test, vi } from "vitest";
import type { InstanceState } from "./bridge-contracts";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, Set<(event: { payload: unknown }) => void>>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, fn: (event: { payload: unknown }) => void) => {
    if (!tauri.listeners.has(name)) tauri.listeners.set(name, new Set());
    tauri.listeners.get(name)!.add(fn);
    return () => tauri.listeners.get(name)!.delete(fn);
  },
}));

const instances = (active: string): InstanceState => ({
  revision: `r-${active}`,
  active,
  machines: [
    { id: "m1", name: "东京", sshHost: "tokyo", fallbackHost: null, useSudo: false },
  ],
  connection: {
    status: active === "local" ? "local" : "connected",
    version: active === "local" ? null : "0.41.0",
    error: null,
    unsupported: [],
  },
});
const emit = (name: string, payload: unknown) =>
  tauri.listeners.get(name)?.forEach((fn) => fn({ payload }));
const listening = (name: string) => tauri.listeners.get(name)?.size ?? 0;
/** A fresh bridge in the desktop app whose `get_instances` answers `initial`. */
async function load(initial: InstanceState | Promise<InstanceState>) {
  tauri.invoke.mockImplementation(async (name: string) =>
    name === "get_instances" ? initial : `${name} done`,
  );
  const bridge = await import("./bridge");
  return bridge;
}
beforeEach(() => {
  vi.resetModules();
  tauri.invoke.mockReset();
  tauri.listeners.clear();
  Object.assign(window, { __TAURI_INTERNALS__: {} });
});

test("desktop commands are invoked by name whichever instance is current", async () => {
  const { command, ready } = await load(instances("m1"));
  await ready;
  await command("set_startup", { enabled: true });
  await command("select_instance", { id: "local", expectedRevision: "r-m1" });
  expect(tauri.invoke).toHaveBeenCalledWith("set_startup", { enabled: true });
  expect(tauri.invoke).toHaveBeenCalledWith("select_instance", {
    id: "local",
    expectedRevision: "r-m1",
  });
  expect(tauri.invoke).not.toHaveBeenCalledWith("remote_call", expect.anything());
});

test("local.command stays on this machine with a server selected", async () => {
  const { local, ready } = await load(instances("m1"));
  await ready;
  await local.command("get_state");
  expect(tauri.invoke).toHaveBeenLastCalledWith("call", {
    name: "get_state",
    args: {},
  });
  expect(tauri.invoke).not.toHaveBeenCalledWith("remote_call", expect.anything());
});

test("shared commands go through call on this machine", async () => {
  const { command } = await load(instances("local"));
  await expect(command("get_gateway", { clientId: "codex" })).resolves.toBe(
    "call done",
  );
  expect(tauri.invoke).toHaveBeenCalledWith("call", {
    name: "get_gateway",
    args: { clientId: "codex" },
  });
  await command("get_state");
  expect(tauri.invoke).toHaveBeenLastCalledWith("call", {
    name: "get_state",
    args: {},
  });
});

test("shared commands go through remote_call on a server and follow a switch", async () => {
  const { command } = await load(instances("m1"));
  await command("get_gateway", { clientId: "codex" });
  expect(tauri.invoke).toHaveBeenLastCalledWith("remote_call", {
    instance: "m1",
    name: "get_gateway",
    args: { clientId: "codex" },
  });
  emit("instance-state", instances("local"));
  await command("get_usage_state");
  expect(tauri.invoke).toHaveBeenLastCalledWith("call", {
    name: "get_usage_state",
    args: {},
  });
});

test("the first shared command waits until the current instance is known", async () => {
  let answer!: (state: InstanceState) => void;
  const { command } = await load(
    new Promise<InstanceState>((resolve) => (answer = resolve)),
  );
  const pending = command("get_state");
  await vi.waitFor(() =>
    expect(tauri.invoke).toHaveBeenCalledWith("get_instances", {}),
  );
  expect(tauri.invoke).toHaveBeenCalledTimes(1);
  answer(instances("m1"));
  await pending;
  expect(tauri.invoke).toHaveBeenLastCalledWith("remote_call", {
    instance: "m1",
    name: "get_state",
    args: {},
  });
});

test("an instance event that arrives before the first read wins over it", async () => {
  let answer!: (state: InstanceState) => void;
  const { command, ready } = await load(
    new Promise<InstanceState>((resolve) => (answer = resolve)),
  );
  await vi.waitFor(() => expect(listening("instance-state")).toBe(1));
  emit("instance-state", instances("m1"));
  answer(instances("local"));
  await ready;
  await command("get_state");
  expect(tauri.invoke).toHaveBeenLastCalledWith(
    "remote_call",
    expect.objectContaining({ instance: "m1" }),
  );
});

test("a failed first read leaves this machine selected", async () => {
  const { command, ready } = await load(Promise.reject(new Error("no such command")));
  await ready;
  await command("get_state");
  expect(tauri.invoke).toHaveBeenLastCalledWith("call", {
    name: "get_state",
    args: {},
  });
});

test("shared events reach the listener only from the current instance", async () => {
  const { subscribe, ready } = await load(instances("local"));
  await ready;
  const seen: unknown[] = [];
  await subscribe("switch-notice", (text) => seen.push(text));
  emit("switch-notice", "local one");
  emit("remote-event", { instance: "m1", event: "switch-notice", payload: "early" });
  expect(seen).toEqual(["local one"]);

  emit("instance-state", instances("m1"));
  emit("switch-notice", "local two");
  emit("remote-event", { instance: "m1", event: "switch-notice", payload: "remote" });
  emit("remote-event", { instance: "m2", event: "switch-notice", payload: "other" });
  emit("remote-event", { instance: "m1", event: "usage-state", payload: null });
  expect(seen).toEqual(["local one", "remote"]);
});

test("unsubscribing from a shared event drops both listeners", async () => {
  const { subscribe, ready } = await load(instances("local"));
  await ready;
  const fn = vi.fn();
  const off = await subscribe("gateway-freeze", fn);
  expect(listening("gateway-freeze")).toBe(1);
  expect(listening("remote-event")).toBe(1);
  off();
  expect(listening("gateway-freeze")).toBe(0);
  expect(listening("remote-event")).toBe(0);
  emit("gateway-freeze", {});
  emit("remote-event", { instance: "local", event: "gateway-freeze", payload: {} });
  expect(fn).not.toHaveBeenCalled();
});

test("desktop events are delivered directly and never read from remote-event", async () => {
  const { subscribe, ready } = await load(instances("m1"));
  await ready;
  const fn = vi.fn();
  const off = await subscribe("login-state", fn);
  expect(listening("remote-event")).toBe(0);
  emit("login-state", { phase: "idle" });
  emit("remote-event", { instance: "m1", event: "login-state", payload: {} });
  expect(fn).toHaveBeenCalledTimes(1);
  expect(fn).toHaveBeenCalledWith({ phase: "idle" });
  off();
  expect(listening("login-state")).toBe(0);
});
