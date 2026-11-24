import { act, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { mock } from "./test-bridge";
vi.mock("./bridge", async () => ({
  ...(await import("./test-bridge")).unfrozenBridge,
  preview: true,
}));
import App from "./App";
import AntigravityAccounts from "./AntigravityAccounts";
import ClaudeProfile from "./ClaudeProfile";
import Gateway from "./Gateway";
import QuickPanel from "./QuickPanel";
import type { InstanceState } from "./bridge-contracts";
import { gatewayDemo } from "./gateway-preview";
import { setInstanceState } from "./instance";
import { demo } from "./preview";

const instances = (active: "local" | "m1"): InstanceState => ({
  revision: `r-${active}`,
  active,
  machines: [
    { id: "m1", name: "东京", sshHost: "tokyo", fallbackHost: null, useSudo: false },
  ],
  connection:
    active === "local"
      ? { status: "local", version: null, error: null, unsupported: [] }
      : { status: "connected", version: "0.41.0", error: null, unsupported: [] },
});
const calls = (name: string) =>
  mock.command.mock.calls.filter(([called]) => called === name);
beforeEach(() => {
  localStorage.clear();
  mock.listeners.clear();
  mock.command.mockReset();
  mock.command.mockImplementation(
    async (name: string, args?: { clientId?: string }) => {
      switch (name) {
        case "get_instances":
          return instances("m1");
        case "get_state":
          return structuredClone(demo);
        case "get_login":
          return { phase: "idle", mode: "", url: null, code: null, message: "" };
        case "get_provider_imports":
          return [];
        case "get_gateway":
          return { ...structuredClone(gatewayDemo), clientId: args?.clientId };
        case "get_quick":
          return { pinned: false, visible: true };
        case "get_clamshell_state":
          return { supported: true, enabled: false, batterySleep: 0, revision: "p" };
        case "get_claude_profile":
          return {
            mode: "official",
            revision: "profile",
            initialized: true,
            conflict: null,
            files: [],
            warnings: [],
          };
        case "claude_login_status":
          return { phase: "idle", authenticated: false, error: null };
        case "get_antigravity_auth_state":
          return {
            revision: "ag",
            oauthPresent: false,
            oauthEmail: null,
            oauthAuthMethod: null,
            appRestarted: false,
            accounts: [],
          };
      }
    },
  );
  setInstanceState(instances("m1"));
});
afterEach(() => localStorage.clear());

it("the Codex account page of a server has no import, export or sign-in entries", async () => {
  render(<App />);
  await screen.findAllByRole("button", { name: /^切换到 / });
  expect(
    screen.queryByRole("group", { name: "账号导入导出" }),
  ).not.toBeInTheDocument();
  for (const name of ["导入", "导出", "添加账号", "设备码刷新"])
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
  expect(screen.getByLabelText("当前实例：东京")).toBeInTheDocument();
});

it("this machine keeps them, and switching instances reads the pages again", async () => {
  setInstanceState(instances("local"));
  render(<App />);
  await screen.findAllByRole("button", { name: /^切换到 / });
  expect(
    within(screen.getByRole("group", { name: "账号导入导出" })).getAllByRole(
      "button",
    ),
  ).toHaveLength(2);
  expect(screen.getByRole("button", { name: "添加账号" })).toBeInTheDocument();
  expect(calls("get_state")).toHaveLength(1);

  act(() => setInstanceState(instances("m1")));
  await waitFor(() => expect(calls("get_state")).toHaveLength(2));
  await waitFor(() =>
    expect(
      screen.queryByRole("button", { name: "添加账号" }),
    ).not.toBeInTheDocument(),
  );
});

it("a server that comes back is read again, one that drops keeps the page", async () => {
  render(<App />);
  await screen.findAllByRole("button", { name: /^切换到 / });
  const offline: InstanceState = {
    ...instances("m1"),
    connection: { status: "offline", version: null, error: "连接已断开", unsupported: [] },
  };
  act(() => setInstanceState(offline));
  expect(calls("get_state")).toHaveLength(1);
  act(() => setInstanceState(instances("m1")));
  await waitFor(() => expect(calls("get_state")).toHaveLength(2));
});

it("Antigravity accounts of a server cannot be imported or exported", async () => {
  render(<AntigravityAccounts notify={() => {}} setError={() => {}} />);
  await screen.findByRole("button", { name: "刷新 Antigravity 认证状态" });
  expect(
    screen.queryByRole("button", { name: "导入 Antigravity 账号" }),
  ).not.toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "导出 Antigravity 账号" }),
  ).not.toBeInTheDocument();
});

it("the official Claude sign-in is left to the server", async () => {
  render(<ClaudeProfile notify={() => {}} />);
  expect(
    await screen.findByText("官方登录请在服务器上完成"),
  ).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /官方登录|已登录官方账号/ })).not.toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "刷新官方登录状态" }),
  ).not.toBeInTheDocument();
  expect(calls("claude_login_status")).toHaveLength(0);
});

it("the agy gateway of a server says it cannot take over and locks the switch", async () => {
  localStorage.setItem("vela.main.client", "agy");
  render(<Gateway notify={() => {}} />);
  expect(
    await screen.findByText("服务器版暂不支持接管 agy"),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "启用" })).toBeDisabled();
});

it("other gateways of a server stay switchable unless the server lacks the command", async () => {
  localStorage.setItem("vela.main.client", "codex");
  const { unmount } = render(<Gateway notify={() => {}} />);
  expect(await screen.findByRole("button", { name: "启用" })).toBeEnabled();
  unmount();

  const limited = instances("m1");
  limited.connection.unsupported = ["start_gateway"];
  setInstanceState(limited);
  render(<Gateway notify={() => {}} />);
  expect(await screen.findByRole("button", { name: "启用" })).toBeDisabled();
});

it("the quick panel of a server offers the instances and drops the controls of this machine", async () => {
  localStorage.setItem("vela.quick.client", "agy");
  render(<QuickPanel />);
  await screen.findByRole("button", { name: "启用网关" });
  expect(screen.getByRole("button", { name: "启用网关" })).toBeDisabled();
  expect(
    within(screen.getByRole("navigation", { name: "实例" })).getByRole(
      "button",
      { name: "东京" },
    ),
  ).toHaveAttribute("aria-pressed", "true");
  expect(screen.queryByRole("button", { name: /^退出 / })).not.toBeInTheDocument();
  expect(screen.queryByLabelText("合盖不休眠")).not.toBeInTheDocument();
  expect(calls("get_clamshell_state")).toHaveLength(0);
});

it("the quick panel of this machine keeps them", async () => {
  setInstanceState(instances("local"));
  render(<QuickPanel />);
  expect(
    await screen.findByRole("button", { name: "退出 Claude Code" }),
  ).toBeInTheDocument();
  await waitFor(() => expect(calls("get_clamshell_state")).not.toHaveLength(0));
});
