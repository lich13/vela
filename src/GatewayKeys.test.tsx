import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent, { type UserEvent } from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { InstanceState } from "./bridge-contracts";
import type { GatewayState } from "./types";
import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).unfrozenBridge);
import Gateway from "./Gateway";
import { gatewayDemo } from "./gateway-preview";
import { setInstanceState } from "./instance";

const instances = (active: "local" | "m1"): InstanceState => ({
  revision: `r-${active}`,
  active,
  machines: [
    {
      id: "m1",
      name: "东京",
      kind: "linux",
      sshHost: "tokyo",
      fallbackHost: null,
      useSudo: false,
    },
  ],
  connection:
    active === "local"
      ? { status: "local", version: null, error: null, unsupported: [] }
      : {
          status: "connected",
          version: "0.44.0",
          error: null,
          unsupported: [],
        },
});
let state: GatewayState;
const notify = vi.fn();
const writes = () =>
  mock.command.mock.calls
    .filter(([name]) => name === "update_gateway")
    .map(([, args]) => args);
/** The page with the block opened: it starts out closed. */
const open = async (user: UserEvent) => {
  render(<Gateway notify={notify} />);
  await screen.findByRole("button", { name: "添加" });
  const block = screen.getByText("访问 Key").closest("details")!;
  expect(block).not.toHaveAttribute("open");
  await user.click(screen.getByText("访问 Key"));
  return within(block);
};
beforeEach(() => {
  localStorage.clear();
  localStorage.setItem("vela.main.client", "codex");
  setInstanceState(instances("local"));
  notify.mockReset();
  state = {
    ...structuredClone(gatewayDemo),
    revision: "r1",
    entryKeys: [
      { id: "k1", name: "团队" },
      { id: "k2", name: "外部工具" },
    ],
  };
  mock.command.mockReset();
  mock.listeners.clear();
  mock.command.mockImplementation(async (name, args) => {
    if (name === "get_gateway") return structuredClone(state);
    if (name === "copy_gateway_key") return null;
    if (name === "update_gateway") {
      if (args.expectedRevision !== state.revision)
        throw {
          code: "CONFLICT",
          message: "网关设置已变化，请使用最新状态重试",
        };
      const { op, id, name: label } = args.edit;
      const keys = state.entryKeys!;
      state = {
        ...state,
        revision: `${state.revision}+`,
        entryKeys:
          op === "deleteEntryKey"
            ? keys.filter((k) => k.id !== id)
            : id
              ? keys.map((k) => (k.id === id ? { ...k, name: label } : k))
              : [...keys, { id: "k3", name: label }],
      };
      return structuredClone(state);
    }
  });
});
afterEach(() => localStorage.clear());

it("lists the client's own key without actions, then every key handed out", async () => {
  const block = await open(userEvent.setup());
  const rows = Array.from(
    block.getByText("本机 Key").closest(".policy-parameters")!.children,
  ) as HTMLElement[];
  expect(rows.map((row) => row.textContent)).toEqual([
    "本机 Key默认",
    "团队",
    "外部工具",
  ]);
  expect(within(rows[0]).queryByRole("button")).not.toBeInTheDocument();
  for (const name of ["团队", "外部工具"])
    for (const action of ["复制", "重命名", "删除"])
      expect(
        block.getByRole("button", { name: `${action} ${name}` }),
      ).toBeEnabled();
  expect(block.getByRole("button", { name: "添加 Key" })).toBeEnabled();
});

it("adds a key under the name typed, against the revision the dialog opened on", async () => {
  const user = userEvent.setup();
  const block = await open(user);
  await user.click(block.getByRole("button", { name: "添加 Key" }));
  const dialog = within(screen.getByRole("dialog", { name: "添加 Key" }));
  await user.click(dialog.getByRole("button", { name: "保存" }));
  expect(dialog.getByRole("alert")).toHaveTextContent("请填写名称");
  expect(writes()).toHaveLength(0);

  await user.type(dialog.getByLabelText("名称"), "  CI  ");
  await user.click(dialog.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  expect(writes()).toEqual([
    {
      clientId: "codex",
      expectedRevision: "r1",
      edit: { op: "saveEntryKey", name: "CI" },
    },
  ]);
  expect(block.getByRole("button", { name: "复制 CI" })).toBeInTheDocument();
});

it("renames a key by its id, and writes nothing for a name left as it was", async () => {
  const user = userEvent.setup();
  const block = await open(user);
  await user.click(block.getByRole("button", { name: "重命名 团队" }));
  await user.click(
    within(screen.getByRole("dialog", { name: "重命名 Key" })).getByRole(
      "button",
      { name: "保存" },
    ),
  );
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  expect(writes()).toHaveLength(0);

  await user.click(block.getByRole("button", { name: "重命名 团队" }));
  const dialog = within(screen.getByRole("dialog", { name: "重命名 Key" }));
  const name = dialog.getByLabelText("名称");
  expect(name).toHaveValue("团队");
  await user.clear(name);
  await user.type(name, "团队 A");
  await user.click(dialog.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  expect(writes()).toEqual([
    {
      clientId: "codex",
      expectedRevision: "r1",
      edit: { op: "saveEntryKey", id: "k1", name: "团队 A" },
    },
  ]);
  expect(
    block.getByRole("button", { name: "删除 团队 A" }),
  ).toBeInTheDocument();
});

it("deletes a key only once the warning is confirmed", async () => {
  const user = userEvent.setup();
  const block = await open(user);
  await user.click(block.getByRole("button", { name: "删除 团队" }));
  let dialog = await screen.findByRole("dialog", { name: "确认操作" });
  expect(dialog).toHaveTextContent(
    "删除 Key「团队」？使用它的请求会立即被拒绝，已有用量记录保留。",
  );
  await user.click(within(dialog).getByRole("button", { name: "取消" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
  expect(writes()).toHaveLength(0);

  await user.click(block.getByRole("button", { name: "删除 团队" }));
  dialog = await screen.findByRole("dialog", { name: "确认操作" });
  await user.click(within(dialog).getByRole("button", { name: "删除" }));
  await waitFor(() =>
    expect(
      block.queryByRole("button", { name: "删除 团队" }),
    ).not.toBeInTheDocument(),
  );
  expect(writes()).toEqual([
    {
      clientId: "codex",
      expectedRevision: "r1",
      edit: { op: "deleteEntryKey", id: "k1" },
    },
  ]);
  expect(block.getByRole("button", { name: "删除 外部工具" })).toBeEnabled();
});

it.each(["local", "m1"] as const)(
  "copies a key of %s through the desktop command and never reads it",
  async (active) => {
    setInstanceState(instances(active));
    const user = userEvent.setup();
    const block = await open(user);
    await user.click(block.getByRole("button", { name: "复制 外部工具" }));
    await waitFor(() => expect(notify).toHaveBeenCalledWith("Key 已复制"));
    expect(mock.command).toHaveBeenCalledWith("copy_gateway_key", {
      clientId: "codex",
      instance: active,
      keyId: "k2",
    });
    expect(writes()).toHaveLength(0);
  },
);

it("says why a copy failed and reports nothing copied", async () => {
  const user = userEvent.setup();
  const block = await open(user);
  const implementation = mock.command.getMockImplementation()!;
  mock.command.mockImplementation(async (name, args) => {
    if (name === "copy_gateway_key") throw new Error("无法写入剪贴板");
    return implementation(name, args);
  });
  await user.click(block.getByRole("button", { name: "复制 团队" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("无法写入剪贴板");
  expect(notify).not.toHaveBeenCalled();
});

it("keeps the name on a conflict and retries against the latest revision", async () => {
  const user = userEvent.setup();
  const block = await open(user);
  await user.click(block.getByRole("button", { name: "添加 Key" }));
  const dialog = within(screen.getByRole("dialog", { name: "添加 Key" }));
  await user.type(dialog.getByLabelText("名称"), "CI");
  // Someone else edits the gateway and the page has not heard of it.
  state = { ...state, revision: "r2" };
  await user.click(dialog.getByRole("button", { name: "保存" }));
  expect(await dialog.findByRole("alert")).toHaveTextContent("网关设置已变化");
  expect(dialog.getByLabelText("名称")).toHaveValue("CI");
  expect(writes()).toEqual([
    {
      clientId: "codex",
      expectedRevision: "r1",
      edit: { op: "saveEntryKey", name: "CI" },
    },
  ]);

  await user.click(dialog.getByRole("button", { name: "重试" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  expect(writes()[1]).toEqual({
    clientId: "codex",
    expectedRevision: "r2",
    edit: { op: "saveEntryKey", name: "CI" },
  });
  expect(block.getByRole("button", { name: "复制 CI" })).toBeInTheDocument();
});
