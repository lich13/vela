import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import type { GatewayFreeze } from "./bridge-contracts";
import { clients } from "./ClientSelection";
const mock = vi.hoisted(() => ({
  command: vi.fn(),
  listeners: new Map<string, (v: unknown) => void>(),
}));
vi.mock("./bridge", () => ({
  command: mock.command,
  subscribe: async (name: string, fn: (v: unknown) => void) => {
    mock.listeners.set(name, fn);
    return () => mock.listeners.delete(name);
  },
}));
import FreezePanel from "./FreezePanel";
const status = {
  frozen: false,
  draining: 0,
  receiving: 0,
  waiting: 0,
  headers: 0,
  streaming: 0,
  finishing: 0,
  queued: 0,
};
const initial: GatewayFreeze = {
  ...status,
  partial: false,
  clients: clients.map((clientId) => ({ ...status, clientId, resumeAt: null })),
};
beforeEach(() => {
  mock.command.mockReset().mockResolvedValue(initial);
  mock.listeners.clear();
});
it("freezes only the selected platform and schedules, changes and cancels its recovery", async () => {
  const user = userEvent.setup();
  render(<FreezePanel />);
  const codex = within(await screen.findByRole("region", { name: "Codex" }));
  const claude = within(screen.getByRole("region", { name: "Claude Code" }));
  expect(codex.getByRole("button", { name: "定时恢复" })).toBeDisabled();
  const frozen: GatewayFreeze = {
    ...initial,
    partial: true,
    clients: initial.clients.map((c) =>
      c.clientId === "codex" ? { ...c, frozen: true } : c,
    ),
  };
  mock.command.mockResolvedValue(frozen);
  await user.click(codex.getByRole("button", { name: "冻结" }));
  expect(mock.command).toHaveBeenLastCalledWith("set_gateway_frozen", {
    frozen: true,
    clientId: "codex",
  });
  expect(await codex.findByText("已冻结")).toBeInTheDocument();
  expect(claude.getByText("未冻结")).toBeInTheDocument();
  const clock = vi
    .spyOn(Date, "now")
    .mockReturnValue(new Date(2025, 1, 3, 4, 5, 6).getTime());
  await user.click(codex.getByRole("button", { name: "定时恢复" }));
  clock.mockRestore();
  expect(
    new Date(
      (codex.getByLabelText("恢复时间") as HTMLInputElement).value,
    ).getTime(),
  ).toBe(new Date(2025, 1, 3, 4, 5, 6).getTime());
  const time = "2099-06-15T18:30:45";
  const at = new Date(time).getTime();
  const scheduled: GatewayFreeze = {
    ...frozen,
    clients: frozen.clients.map((c) =>
      c.clientId === "codex" ? { ...c, resumeAt: at } : c,
    ),
  };
  mock.command.mockResolvedValue(scheduled);
  fireEvent.change(codex.getByLabelText("恢复时间"), {
    target: { value: time },
  });
  await user.click(codex.getByRole("button", { name: "保存" }));
  expect(mock.command).toHaveBeenLastCalledWith("set_gateway_freeze_resume", {
    clientId: "codex",
    resumeAt: at,
  });
  expect(await codex.findByText("2099-06-15 18:30:45")).toBeInTheDocument();
  await user.click(codex.getByRole("button", { name: "修改时间" }));
  expect(
    new Date(
      (codex.getByLabelText("恢复时间") as HTMLInputElement).value,
    ).getTime(),
  ).toBe(at);
  await user.click(codex.getByRole("button", { name: /^取消$/ }));
  mock.command.mockResolvedValue(frozen);
  await user.click(codex.getByRole("button", { name: "取消 Codex 定时恢复" }));
  expect(mock.command).toHaveBeenLastCalledWith("set_gateway_freeze_resume", {
    clientId: "codex",
    resumeAt: null,
  });
  act(() => mock.listeners.get("gateway-freeze")?.(initial));
  expect(await codex.findByText("未冻结")).toBeInTheDocument();
});
it("keeps draining controls disabled and retains failed schedule input for retry", async () => {
  const user = userEvent.setup();
  mock.command.mockResolvedValue({
    ...initial,
    partial: true,
    clients: initial.clients.map((c) => ({
      ...c,
      frozen: true,
      draining: 1,
      streaming: 1,
    })),
  });
  render(<FreezePanel />);
  const codex = within(await screen.findByRole("region", { name: "Codex" }));
  expect(codex.getByRole("button", { name: "解冻" })).toBeDisabled();
  expect(codex.getByRole("button", { name: "定时恢复" })).toBeDisabled();
  act(() =>
    mock.listeners.get("gateway-freeze")?.({
      ...initial,
      partial: true,
      clients: initial.clients.map((c) => ({ ...c, frozen: true })),
    }),
  );
  await user.click(codex.getByRole("button", { name: "定时恢复" }));
  fireEvent.change(codex.getByLabelText("恢复时间"), {
    target: { value: "2000-01-01T00:00" },
  });
  await user.click(codex.getByRole("button", { name: "保存" }));
  expect(await codex.findByRole("alert")).toHaveTextContent("晚于当前时间");
  fireEvent.change(codex.getByLabelText("恢复时间"), {
    target: { value: "2099-01-01T00:00" },
  });
  mock.command.mockRejectedValue({ message: "fixture failure" });
  await user.click(codex.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(codex.getByRole("alert")).toHaveTextContent("fixture failure"),
  );
  expect(codex.getByLabelText("恢复时间")).toHaveValue("2099-01-01T00:00");
});
