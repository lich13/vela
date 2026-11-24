import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
vi.mock("./confirmation", () => ({ confirmAction: mock.confirm }));
import InstanceSwitch, { QuickInstances } from "./InstanceSwitch";
import type { InstanceState, Machine } from "./bridge-contracts";
import { instanceState, setInstanceState } from "./instance";

const tokyo: Machine = {
  id: "m1",
  name: "东京",
  sshHost: "tokyo",
  fallbackHost: null,
  useSudo: false,
};
const connection = (
  status: InstanceState["connection"]["status"],
  error: string | null = null,
): InstanceState["connection"] => ({
  status,
  version: status === "connected" ? "0.41.0" : null,
  error,
  unsupported: [],
});
/** What the desktop app holds; the commands below answer from it. */
let server: InstanceState;
let failure: Partial<Record<string, { code: string; message: string }>>;
let revision: number;
function serve(patch: Partial<InstanceState> = {}) {
  server = {
    revision: "r1",
    active: "local",
    machines: [],
    connection: connection("local"),
    ...patch,
  };
  setInstanceState(server);
}
const commit = (patch: Partial<InstanceState>) =>
  (server = { ...server, ...patch, revision: `r${++revision}` });
const calls = (name: string) =>
  mock.command.mock.calls.filter(([called]) => called === name);
const confirm = vi.fn(async () => true);
const openMenu = (user: ReturnType<typeof userEvent.setup>) =>
  user.click(screen.getByLabelText(/^当前实例/));
async function manage(user: ReturnType<typeof userEvent.setup>) {
  render(<InstanceSwitch confirm={confirm} />);
  await openMenu(user);
  await user.click(screen.getByRole("button", { name: "管理实例" }));
  return within(await screen.findByRole("dialog"));
}
beforeEach(() => {
  revision = 1;
  failure = {};
  confirm.mockClear();
  mock.confirm.mockReset();
  mock.command.mockReset();
  mock.command.mockImplementation(
    async (name: string, args: Record<string, unknown>) => {
      if (failure[name]) throw failure[name];
      if (name === "get_instances") return server;
      if (name === "test_instance") return { version: "0.41.0" };
      if (args.expectedRevision !== server.revision)
        throw { code: "CONFLICT", message: "实例列表已更新，请重试" };
      if (name === "save_instance") {
        const machine = args.machine as Machine & { id: string | null };
        const saved = { ...machine, id: machine.id ?? "m-new" };
        return commit({
          machines: machine.id
            ? server.machines.map((m) => (m.id === saved.id ? saved : m))
            : [...server.machines, saved],
        });
      }
      if (name === "delete_instance")
        return commit({
          machines: server.machines.filter((m) => m.id !== args.id),
          ...(server.active === args.id
            ? { active: "local", connection: connection("local") }
            : {}),
        });
      if (name === "select_instance")
        return commit({
          active: String(args.id),
          connection: connection(args.id === "local" ? "local" : "connected"),
        });
    },
  );
  serve();
});

it("shows this machine alone, without a connection status, and still offers the manage dialog", async () => {
  const user = userEvent.setup();
  const dialog = await manage(user);
  expect(screen.getByLabelText("当前实例：本机")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "本机" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  expect(screen.queryByText(/已连接|连接中|离线/)).not.toBeInTheDocument();
  expect(dialog.getByRole("heading", { name: "管理实例" })).toBeInTheDocument();
  expect(dialog.getByText(/还没有服务器/)).toBeInTheDocument();
});

it("adds a server only once both aliases pass the rule", async () => {
  const user = userEvent.setup();
  const dialog = await manage(user);
  await user.click(dialog.getByRole("button", { name: "添加服务器" }));
  await user.type(dialog.getByLabelText("名称"), " 东京 ");
  await user.type(dialog.getByLabelText(/^SSH 主机别名/), "-oProxyCommand");
  await user.click(dialog.getByRole("button", { name: "保存" }));
  expect(dialog.getByRole("alert")).toHaveTextContent(
    "SSH 主机别名只能包含字母、数字、点、下划线和连字符，且不能以连字符开头",
  );

  await user.clear(dialog.getByLabelText(/^SSH 主机别名/));
  await user.type(dialog.getByLabelText(/^SSH 主机别名/), "tokyo");
  await user.type(dialog.getByLabelText("备用别名"), "tokyo lan");
  await user.click(dialog.getByRole("button", { name: "保存" }));
  expect(dialog.getByRole("alert")).toHaveTextContent(/^备用别名只能包含/);
  expect(calls("save_instance")).toHaveLength(0);

  await user.clear(dialog.getByLabelText("备用别名"));
  await user.click(dialog.getByLabelText(/^使用 sudo/));
  await user.click(dialog.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(calls("save_instance")).toEqual([
      [
        "save_instance",
        {
          expectedRevision: "r1",
          machine: {
            id: null,
            name: "东京",
            sshHost: "tokyo",
            fallbackHost: null,
            useSudo: true,
          },
        },
      ],
    ]),
  );
  const list = await dialog.findByRole("list", { name: "服务器" });
  expect(within(list).getByText("东京")).toBeInTheDocument();
  expect(within(list).getByText("tokyo")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "东京" })).toHaveAttribute(
    "aria-pressed",
    "false",
  );
});

it("requires a name", async () => {
  const user = userEvent.setup();
  const dialog = await manage(user);
  await user.click(dialog.getByRole("button", { name: "添加服务器" }));
  await user.type(dialog.getByLabelText(/^SSH 主机别名/), "tokyo");
  await user.click(dialog.getByRole("button", { name: "保存" }));
  expect(dialog.getByRole("alert")).toHaveTextContent("请填写名称");
  expect(calls("save_instance")).toHaveLength(0);
});

it("edits a server in place", async () => {
  serve({ machines: [tokyo] });
  const user = userEvent.setup();
  const dialog = await manage(user);
  await user.click(dialog.getByRole("button", { name: "编辑 东京" }));
  expect(dialog.getByRole("heading", { name: "编辑服务器" })).toBeInTheDocument();
  expect(dialog.getByLabelText("名称")).toHaveValue("东京");
  expect(dialog.getByLabelText(/^SSH 主机别名/)).toHaveValue("tokyo");
  await user.type(dialog.getByLabelText("备用别名"), "tokyo-lan");
  await user.click(dialog.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(calls("save_instance")[0][1]).toEqual({
      expectedRevision: "r1",
      machine: { ...tokyo, fallbackHost: "tokyo-lan" },
    }),
  );
  expect(await dialog.findByText("tokyo · 备用 tokyo-lan")).toBeInTheDocument();
});

it("tests the connection with the typed values and shows the server version", async () => {
  const user = userEvent.setup();
  const dialog = await manage(user);
  await user.click(dialog.getByRole("button", { name: "添加服务器" }));
  await user.type(dialog.getByLabelText(/^SSH 主机别名/), "tokyo");
  await user.type(dialog.getByLabelText("备用别名"), "tokyo-lan");
  await user.click(dialog.getByRole("button", { name: "测试连接" }));
  expect(await dialog.findByRole("status")).toHaveTextContent(
    "连接成功，服务器版本 v0.41.0",
  );
  expect(calls("test_instance")).toEqual([
    [
      "test_instance",
      { sshHost: "tokyo", fallbackHost: "tokyo-lan", useSudo: false },
    ],
  ]);
  // The result describes the values it was run with.
  await user.type(dialog.getByLabelText(/^SSH 主机别名/), "2");
  expect(dialog.queryByRole("status")).not.toBeInTheDocument();
});

it("shows why a connection test failed and never tests an invalid alias", async () => {
  failure.test_instance = { code: "SSH", message: "无法连接 tokyo：连接超时" };
  const user = userEvent.setup();
  const dialog = await manage(user);
  await user.click(dialog.getByRole("button", { name: "添加服务器" }));
  await user.type(dialog.getByLabelText(/^SSH 主机别名/), "to kyo");
  await user.click(dialog.getByRole("button", { name: "测试连接" }));
  expect(dialog.getByRole("alert")).toHaveTextContent(/^SSH 主机别名只能包含/);
  expect(calls("test_instance")).toHaveLength(0);

  await user.clear(dialog.getByLabelText(/^SSH 主机别名/));
  await user.type(dialog.getByLabelText(/^SSH 主机别名/), "tokyo");
  await user.click(dialog.getByRole("button", { name: "测试连接" }));
  expect(await dialog.findByRole("alert")).toHaveTextContent(
    "无法连接 tokyo：连接超时",
  );
  expect(dialog.queryByRole("status")).not.toBeInTheDocument();
});

it("reads the list again when it changed elsewhere", async () => {
  serve({ machines: [tokyo] });
  const user = userEvent.setup();
  const dialog = await manage(user);
  await waitFor(() => expect(calls("get_instances")).toHaveLength(1));
  // Another window renamed the machine after this one read the list.
  commit({ machines: [{ ...tokyo, name: "东京二号" }] });
  await user.click(dialog.getByRole("button", { name: "删除 东京" }));
  expect(await dialog.findByRole("alert")).toHaveTextContent(
    "实例列表已更新，请重试",
  );
  expect(calls("get_instances")).toHaveLength(2);
  await user.click(await dialog.findByRole("button", { name: "删除 东京二号" }));
  await waitFor(() => expect(instanceState().machines).toEqual([]));
});

it("asks before it deletes the server in use", async () => {
  serve({ active: "m1", machines: [tokyo], connection: connection("connected") });
  const user = userEvent.setup();
  const dialog = await manage(user);
  mock.confirm.mockResolvedValueOnce(false);
  await user.click(dialog.getByRole("button", { name: "删除 东京" }));
  expect(mock.confirm).toHaveBeenCalledWith(
    "「东京」正在使用中，删除后会切回本机。",
    "删除",
  );
  expect(calls("delete_instance")).toHaveLength(0);

  mock.confirm.mockResolvedValueOnce(true);
  await user.click(dialog.getByRole("button", { name: "删除 东京" }));
  await waitFor(() => expect(instanceState().active).toBe("local"));
  expect(calls("delete_instance")).toEqual([
    ["delete_instance", { id: "m1", expectedRevision: "r1" }],
  ]);
});

it("deletes an unused server without asking", async () => {
  serve({ machines: [tokyo] });
  const user = userEvent.setup();
  const dialog = await manage(user);
  await user.click(dialog.getByRole("button", { name: "删除 东京" }));
  await waitFor(() => expect(instanceState().machines).toEqual([]));
  expect(mock.confirm).not.toHaveBeenCalled();
});

it("switches to a server and back, showing its connection", async () => {
  serve({ machines: [tokyo] });
  const user = userEvent.setup();
  render(<InstanceSwitch confirm={confirm} />);
  await openMenu(user);
  await user.click(screen.getByRole("button", { name: "东京" }));
  expect(await screen.findByRole("status")).toHaveTextContent("已连接");
  expect(calls("select_instance")).toEqual([
    ["select_instance", { id: "m1", expectedRevision: "r1" }],
  ]);
  expect(confirm).toHaveBeenCalledTimes(1);
  expect(instanceState().active).toBe("m1");
  expect(screen.getByLabelText("当前实例：东京")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "东京" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );

  await user.click(screen.getByRole("button", { name: "本机" }));
  await waitFor(() => expect(instanceState().active).toBe("local"));
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});

it("stays where it is when the drafts are kept", async () => {
  serve({ machines: [tokyo] });
  confirm.mockResolvedValueOnce(false);
  const user = userEvent.setup();
  render(<InstanceSwitch confirm={confirm} />);
  await openMenu(user);
  await user.click(screen.getByRole("button", { name: "东京" }));
  expect(confirm).toHaveBeenCalled();
  expect(calls("select_instance")).toHaveLength(0);
  expect(instanceState().active).toBe("local");
});

it("shows connecting while the server answers and the reason when it does not", async () => {
  serve({ machines: [tokyo] });
  let reject!: (error: unknown) => void;
  const original = mock.command.getMockImplementation()!;
  mock.command.mockImplementation((name: string, args: unknown) =>
    name === "select_instance"
      ? new Promise((_, no) => (reject = no))
      : original(name, args),
  );
  const user = userEvent.setup();
  render(<InstanceSwitch confirm={confirm} />);
  await openMenu(user);
  await user.click(screen.getByRole("button", { name: "东京" }));
  expect(await screen.findByRole("status")).toHaveTextContent("连接中");
  expect(screen.getByRole("button", { name: "本机" })).toBeDisabled();
  reject({ code: "SSH", message: "握手失败：服务器版本过旧" });
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "握手失败：服务器版本过旧",
  );
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(instanceState().active).toBe("local");
});

it("retries an offline server", async () => {
  serve({
    active: "m1",
    machines: [tokyo],
    connection: connection("offline", "连接已断开"),
  });
  const user = userEvent.setup();
  render(<InstanceSwitch confirm={confirm} />);
  expect(screen.getByRole("status")).toHaveTextContent("离线");
  expect(screen.getByRole("alert")).toHaveTextContent("连接已断开");
  await user.click(screen.getByRole("button", { name: "重试" }));
  await waitFor(() =>
    expect(screen.getByRole("status")).toHaveTextContent("已连接"),
  );
  expect(calls("select_instance")).toEqual([
    ["select_instance", { id: "m1", expectedRevision: "r1" }],
  ]);
  // Retrying the instance already shown leaves no draft behind to ask about.
  expect(confirm).not.toHaveBeenCalled();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();
});

it("the quick panel offers instances only once a server exists", async () => {
  const user = userEvent.setup();
  const { container, unmount } = render(<QuickInstances />);
  expect(container).toBeEmptyDOMElement();
  unmount();

  serve({ machines: [tokyo] });
  render(<QuickInstances />);
  const segments = within(screen.getByRole("navigation", { name: "实例" }));
  expect(segments.getByRole("button", { name: "本机" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await user.click(segments.getByRole("button", { name: "东京" }));
  await waitFor(() =>
    expect(segments.getByRole("button", { name: "东京" })).toHaveAttribute(
      "aria-pressed",
      "true",
    ),
  );
  expect(calls("select_instance")).toEqual([
    ["select_instance", { id: "m1", expectedRevision: "r1" }],
  ]);
});

it("the quick panel reports an offline server and retries it", async () => {
  serve({
    active: "m1",
    machines: [tokyo],
    connection: connection("offline", "连接已断开"),
  });
  const user = userEvent.setup();
  render(<QuickInstances />);
  expect(screen.getByRole("alert")).toHaveTextContent("离线：连接已断开");
  await user.click(screen.getByRole("button", { name: "重试" }));
  await waitFor(() =>
    expect(screen.queryByRole("alert")).not.toBeInTheDocument(),
  );
  expect(instanceState().connection.status).toBe("connected");
});
