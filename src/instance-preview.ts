import type { InstanceState, Machine } from "./bridge-contracts";

const local = (): InstanceState["connection"] => ({
  status: "local",
  version: null,
  error: null,
  unsupported: [],
});
let state: InstanceState = {
  revision: "instances-preview",
  active: "local",
  machines: [],
  connection: local(),
};
export const instanceCommands = [
  "get_instances",
  "save_instance",
  "delete_instance",
  "select_instance",
  "test_instance",
];
export function instancePreview(
  name: string,
  args: Record<string, unknown>,
  emit: (name: string, payload: unknown) => void,
): unknown {
  if (name === "get_instances") return structuredClone(state);
  if (name === "test_instance") {
    // An alias named `unreachable` previews the failure.
    if (args.sshHost === "unreachable")
      throw { code: "SSH", message: "无法连接 unreachable：连接超时" };
    return { version: "0.41.0" };
  }
  if (args.expectedRevision !== state.revision)
    throw { code: "CONFLICT", message: "实例列表已更新，请重试" };
  let { active, machines } = state;
  if (name === "save_instance") {
    const machine = args.machine as Omit<Machine, "id"> & { id: string | null };
    const saved = { ...machine, id: machine.id ?? crypto.randomUUID() };
    machines = machine.id
      ? machines.map((m) => (m.id === saved.id ? saved : m))
      : [...machines, saved];
  } else if (name === "delete_instance") {
    machines = machines.filter((m) => m.id !== args.id);
    if (active === args.id) active = "local";
  } else active = String(args.id);
  state = {
    revision: crypto.randomUUID(),
    active,
    machines,
    connection:
      active === "local"
        ? local()
        : { ...local(), status: "connected", version: "0.41.0" },
  };
  emit("instance-state", structuredClone(state));
  return structuredClone(state);
}
