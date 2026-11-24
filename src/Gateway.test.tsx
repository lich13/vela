import { render, screen, waitFor, act, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import type { ClientId, GatewayState, ProviderQuota } from "./types";
import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).unfrozenBridge);
import Gateway from "./Gateway";
import { claudeGatewayDemo, gatewayDemo } from "./gateway-preview";
import { providerTestingPreview } from "./provider-testing-preview";
let state: GatewayState;
let claudeState: GatewayState;
beforeEach(() => {
  localStorage.clear();
  state = structuredClone(gatewayDemo);
  claudeState = structuredClone(claudeGatewayDemo);
  mock.command.mockReset();
  mock.listeners.clear();
  mock.command.mockImplementation(
    async (name: string, args?: { clientId?: ClientId }) => {
      if (name.startsWith("provider_test_"))
        return providerTestingPreview(name, args ?? {});
      if (name === "get_gateway" || name === "update_gateway")
        return structuredClone(
          args?.clientId === "claude" ? claudeState : state,
        );
    },
  );
});
afterEach(() => localStorage.clear());
describe("gateway controls", () => {
  it.each(["codex", "claude"] as const)(
    "%s preserves a conflicting provider draft and retries only with a fresh version",
    async (client) => {
      localStorage.setItem("vela.main.client", client);
      const user = userEvent.setup();
      let current = client === "claude" ? claudeState : state;
      current.running = true;
      current.mode = "auto";
      mock.command.mockImplementation(async (name, args) => {
        if (name === "get_gateway") return structuredClone(current);
        if (name === "update_gateway") {
          if (args.expectedRevision !== current.revision)
            throw {
              code: "CONFLICT",
              message: "网关设置已变化，请使用最新状态重试",
            };
          return structuredClone(current);
        }
      });
      render(<Gateway notify={() => {}} />);
      // Explicit UI selection also covers independent client preferences.
      await screen.findByRole("button", { name: "添加" });
      await user.click(screen.getByRole("button", { name: "添加" }));
      const address = screen.getByLabelText(
        client === "codex" ? "base_url" : "ANTHROPIC_BASE_URL",
      );
      const key = screen.getByLabelText(
        client === "codex"
          ? "experimental_bearer_token"
          : "ANTHROPIC_AUTH_TOKEN",
      );
      await user.type(address, "https://new.invalid/v1");
      await user.type(key, "fixture-key");
      const original = current.revision;
      // Runtime events keep the same semantic version and cannot erase input.
      act(() =>
        mock.listeners.get("gateway-state")!({
          ...current,
          lastSuccessful: "backup",
          waitingRequests: 2,
        }),
      );
      expect(key).toHaveValue("fixture-key");
      current = { ...current, revision: "external-edit" };
      act(() => mock.listeners.get("gateway-state")!(current));
      await user.click(screen.getByRole("button", { name: "保存" }));
      expect(await screen.findByRole("alert")).toHaveTextContent(
        "网关设置已变化",
      );
      expect(mock.command).toHaveBeenCalledWith(
        "update_gateway",
        expect.objectContaining({ expectedRevision: original }),
      );
      expect(key).toHaveValue("fixture-key");
      const writes = () =>
        mock.command.mock.calls.filter(([name]) => name === "update_gateway");
      expect(writes()).toHaveLength(1);
      // The authoritative version can change again without delivering a frontend event.
      current = { ...current, revision: "latest-on-retry" };
      await user.click(screen.getByRole("button", { name: "重试" }));
      await waitFor(() =>
        expect(
          screen.queryByRole("dialog", { name: "添加供应商" }),
        ).not.toBeInTheDocument(),
      );
      expect(writes()).toHaveLength(2);
      expect(writes()[1][1]).toMatchObject({
        clientId: client,
        expectedRevision: "latest-on-retry",
        edit: {
          op: "saveProvider",
          bufferCompleteResponse: false,
          token: "fixture-key",
          baseUrl: "https://new.invalid/v1",
        },
      });
    },
  );

  it("retries advanced settings without losing typed parameters", async () => {
    const user = userEvent.setup();
    let current = structuredClone(state);
    mock.command.mockImplementation(async (name, args) => {
      if (name === "get_gateway") return structuredClone(current);
      if (name === "update_gateway") {
        if (args.expectedRevision !== current.revision)
          throw { code: "CONFLICT", message: "设置已变化" };
        current = {
          ...current,
          settings: args.edit.settings,
          revision: "saved",
        };
        return structuredClone(current);
      }
    });
    render(<Gateway notify={() => {}} />);
    await screen.findByRole("button", { name: "添加" });
    await user.click(screen.getByText("故障策略"));
    const input = screen.getByLabelText("容量错误等待 / 秒");
    await user.clear(input);
    await user.type(input, "120");
    current = { ...current, revision: "external" };
    act(() => mock.listeners.get("gateway-state")!(current));
    await user.click(screen.getByRole("button", { name: "保存参数" }));
    expect(
      (await screen.findAllByRole("alert")).some((e) =>
        e.textContent?.includes("设置已变化"),
      ),
    ).toBe(true);
    expect(input).toHaveValue(120);
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() =>
      expect(mock.command).toHaveBeenCalledWith(
        "update_gateway",
        expect.objectContaining({
          expectedRevision: "external",
          edit: {
            op: "settings",
            settings: expect.objectContaining({ capacityRetrySeconds: 120 }),
          },
        }),
      ),
    );
  });
  it("resets a provider from the row and renames it in the API tab", async () => {
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    const { id, name } = state.providers[0];
    const title = await screen.findByText(name, {
      selector: ".provider-title",
    });
    // The name is no longer edited in place.
    await user.dblClick(title);
    expect(
      screen.queryByRole("textbox", { name: `${name} 名称` }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: `${name} 编辑 API` }));
    const dialog = within(
      await screen.findByRole("dialog", { name: `编辑供应商 · ${name}` }),
    );
    const input = dialog.getByRole("textbox", { name: "名称" });
    expect(input).toHaveValue(name);
    await user.clear(input);
    await user.type(input, "新的供应商");
    await user.click(dialog.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: /^编辑供应商/ }),
      ).not.toBeInTheDocument(),
    );
    const edits = mock.command.mock.calls
      .filter(([command]) => command === "update_gateway")
      .map(([, args]) => args);
    expect(edits).toHaveLength(2);
    expect(edits[0]).toEqual({
      clientId: "codex",
      edit: { op: "renameProvider", id, name: "新的供应商" },
      expectedRevision: state.revision,
    });
    expect(edits[1].edit).toMatchObject({ op: "saveProvider", id });
    expect(edits[1].edit).not.toHaveProperty("name");
    await user.click(
      screen.getByRole("button", {
        name: `${state.providers[0].name} 重置熔断`,
      }),
    );
    expect(mock.command).toHaveBeenCalledWith("update_gateway", {
      clientId: "codex",
      edit: { op: "reset", id: state.providers[0].id },
      expectedRevision: state.revision,
    });
    expect(screen.queryByText("测试连接")).not.toBeInTheDocument();
  });

  it("keeps the provider quota beside the name in the main panel", async () => {
    const p = state.providers[0];
    p.quota = {
      providerId: p.id,
      version: p.quotaVersion,
      state: "ok",
      source: "sub2api",
      checkedAt: 1,
      successAt: 1,
      retryAt: null,
      stale: false,
      error: null,
      keyStatus: null,
      plans: [
        {
          name: "API Key 配额",
          remaining: 8,
          used: 2,
          total: 10,
          unit: "USD",
          unlimited: false,
          resetAt: null,
        },
      ],
      subscriptions: [
        {
          name: "Pro",
          status: "active",
          expiresAt: null,
          active: true,
          plans: [],
        },
      ],
      expiresAt: null,
      expiresAtUnix: null,
      today: null,
      totalUsage: null,
    } as ProviderQuota;
    render(<Gateway notify={() => {}} />);
    const name = await screen.findByText(p.name, {
      selector: ".provider-title",
    });
    const row = name.closest("[data-provider-id]");
    expect(row).not.toBeNull();
    const quota = within(row as HTMLElement).getByLabelText(`${p.name} 额度`);
    expect(
      name.compareDocumentPosition(quota) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(within(quota).getByText("Pro")).toHaveClass("quota-subscription");
  });

  it("consumes explicit provider focus after opening the requested editor", async () => {
    const handled = vi.fn();
    const providerId = state.providers[0].id;
    const { rerender } = render(
      <Gateway
        notify={() => {}}
        focusProvider={{ id: providerId, clientId: "codex", sequence: 9 }}
        onFocusHandled={handled}
      />,
    );
    expect(
      await screen.findByRole("dialog", {
        name: `编辑供应商 · ${state.providers[0].name}`,
      }),
    ).toBeInTheDocument();
    expect(
      within(screen.getByRole("navigation", { name: "供应商设置" })).getByRole(
        "button",
        { name: "白名单" },
      ),
    ).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("group", { name: "模型规则" })).toBeInTheDocument();
    expect(handled).toHaveBeenCalledTimes(1);
    rerender(
      <Gateway
        notify={() => {}}
        focusProvider={null}
        onFocusHandled={handled}
      />,
    );
    expect(
      screen.getByRole("dialog", {
        name: `编辑供应商 · ${state.providers[0].name}`,
      }),
    ).toBeInTheDocument();
    expect(handled).toHaveBeenCalledTimes(1);
  });

  it("starts a fresh provider policy editor after explicit navigation", async () => {
    const providerId = state.providers[0].id;
    const { rerender } = render(
      <Gateway
        notify={() => {}}
        focusProvider={{ id: providerId, clientId: "codex", sequence: 1 }}
      />,
    );
    expect(
      await screen.findByRole("dialog", {
        name: `编辑供应商 · ${state.providers[0].name}`,
      }),
    ).toBeInTheDocument();
    // App confirms discarding before emitting this new navigation sequence.
    rerender(
      <Gateway
        notify={() => {}}
        focusProvider={{ id: providerId, clientId: "codex", sequence: 2 }}
      />,
    );
    expect(
      await screen.findByRole("dialog", {
        name: `编辑供应商 · ${state.providers[0].name}`,
      }),
    ).toBeInTheDocument();
    expect(
      mock.command.mock.calls.some(([name]) => name === "update_gateway"),
    ).toBe(false);
  });

  it("waits for the requested client before consuming connection-test navigation", async () => {
    const handled = vi.fn();
    const { rerender } = render(
      <Gateway notify={() => {}} onFocusHandled={handled} />,
    );
    await screen.findByText(state.providers[0].name, {
      selector: ".provider-title",
    });
    const target = claudeState.providers[0];
    rerender(
      <Gateway
        notify={() => {}}
        focusProvider={{
          id: target.id,
          clientId: "claude",
          action: "testing",
          sequence: 3,
        }}
        onFocusHandled={handled}
      />,
    );
    const dialog = await screen.findByRole("dialog", {
      name: `${target.name} · 测试`,
    });
    expect(
      within(dialog).getByRole("tab", { name: "连接测试" }),
    ).toHaveAttribute("aria-selected", "true");
    expect(handled).toHaveBeenCalledTimes(1);
    expect(mock.command).toHaveBeenCalledWith("provider_test_state", {
      clientId: "claude",
      providerId: target.id,
    });
    expect(
      mock.command.mock.calls.some(([name]) => name === "provider_test_start"),
    ).toBe(false);
  });

  it("accepts an optional display name without adding model settings", async () => {
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await user.click(await screen.findByRole("button", { name: "添加" }));
    await user.type(screen.getByLabelText("名称"), "主力供应商");
    await user.type(
      screen.getByLabelText("base_url"),
      "https://new.example.com/sub/v1",
    );
    const token = screen.getByLabelText("experimental_bearer_token");
    expect(token).toHaveAttribute("type", "password");
    await user.type(token, "fixture-token");
    act(() =>
      mock.listeners.get("gateway-state")!({
        ...state,
        revision: "changed-while-editing",
      }),
    );
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(mock.command).toHaveBeenCalledWith("update_gateway", {
        clientId: "codex",
        edit: {
          op: "saveProvider",
          bufferCompleteResponse: false,
          id: null,
          name: "主力供应商",
          baseUrl: "https://new.example.com/sub/v1",
          token: "fixture-token",
          endlessMode: false,
          upstreamRetrySeconds: 33,
          limitRetrySeconds: 33,
          limitRestSeconds: 8,
          normalRetrySeconds: 3,
          riskCodes: [],
          retryIntensity: 5,
          takeCompaction: true,
          compactionModel: "",
          compactionEffort: "",
          takeAuxiliary: true,
          auxiliaryModel: "",
          auxiliaryEffort: "",
          takeWebSearch: true,
          webSearchModel: "",
          webSearchEffort: "",
        },
        expectedRevision: state.revision,
      }),
    );
  });
  it("keeps a failed provider draft open with its error", async () => {
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await user.click(await screen.findByRole("button", { name: "添加" }));
    await user.type(
      screen.getByLabelText("base_url"),
      "http://127.0.0.1:15722/v1",
    );
    await user.type(
      screen.getByLabelText("experimental_bearer_token"),
      "fixture",
    );
    mock.command.mockRejectedValueOnce({
      code: "LOOP",
      message: "上游不能指向本网关",
    });
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "上游不能指向本网关",
    );
    expect(screen.getByLabelText("base_url")).toHaveValue(
      "http://127.0.0.1:15722/v1",
    );
  });
  it("switches a stopped provider with the visible config revision and restart feedback", async () => {
    const notify = vi.fn();
    const user = userEvent.setup();
    render(<Gateway notify={notify} />);
    await user.click(await screen.findByRole("button", { name: "选择" }));
    expect(mock.command).toHaveBeenCalledWith("update_gateway", {
      clientId: "codex",
      edit: { op: "select", id: "backup" },
      expectedRevision: state.revision,
      expectedConfigRevision: state.configRevision,
    });
    expect(notify).toHaveBeenCalledWith("文件已切换，请重新打开 Codex");
  });
});

it("keeps model choices and manual entries when discovery fails and events refresh", async () => {
  const user = userEvent.setup();
  let discoveryFailed = false;
  mock.command.mockImplementation(async (name: string) => {
    if (name === "get_gateway") return structuredClone(state);
    if (name === "list_provider_models") {
      if (discoveryFailed)
        throw { code: "NETWORK", message: "模型列表读取失败" };
      return { models: ["gpt-A", "gpt-B"], error: null, stale: false };
    }
    if (name === "update_gateway")
      throw { code: "CONFLICT", message: "设置已变化" };
  });
  render(<Gateway notify={() => {}} />);
  await user.click(
    within(await screen.findByLabelText("api.example.com 操作")).getByRole(
      "button",
      { name: "api.example.com 编辑 API" },
    ),
  );
  await user.click(
    within(screen.getByRole("navigation", { name: "供应商设置" })).getByRole(
      "button",
      { name: "白名单" },
    ),
  );
  await user.click(await screen.findByRole("checkbox", { name: "gpt-A" }));
  await user.type(
    screen.getByRole("textbox", { name: "手动添加模型 ID" }),
    "custom-model",
  );
  await user.click(screen.getByRole("button", { name: "添加到白名单" }));
  act(() =>
    mock.listeners.get("gateway-state")!({
      ...state,
      revision: "changed",
      activeConnections: 2,
    }),
  );
  expect(screen.getByRole("checkbox", { name: "gpt-A" })).toBeChecked();
  expect(screen.getByRole("checkbox", { name: "custom-model" })).toBeChecked();
  discoveryFailed = true;
  await user.click(screen.getByRole("button", { name: "刷新模型列表" }));
  const dialog = screen.getByRole("dialog", {
    name: `编辑供应商 · ${state.providers[0].name}`,
  });
  expect(await within(dialog).findByRole("status")).toHaveTextContent(
    "模型列表读取失败",
  );
  expect(screen.getByRole("checkbox", { name: "gpt-A" })).toBeChecked();
  expect(screen.getByRole("checkbox", { name: "custom-model" })).toBeChecked();
  await user.click(screen.getByRole("button", { name: "保存" }));
  expect(await screen.findByText("设置已变化")).toBeInTheDocument();
  expect(screen.getByRole("checkbox", { name: "custom-model" })).toBeChecked();
  expect(mock.command).toHaveBeenCalledWith("update_gateway", {
    clientId: "codex",
    edit: {
      op: "policyProvider",
      id: "primary",
      allowedModels: ["gpt-A", "custom-model"],
    },
    expectedRevision: state.revision,
  });
});

it("refreshes authoritative configuration after a cap conflict, preserves input and permits explicit retry", async () => {
  const user = userEvent.setup();
  let attempts = 0;
  mock.command.mockImplementation(
    async (
      name: string,
      args?: { edit: { maxConcurrency: number }; expectedRevision: string },
    ) => {
      if (name === "get_gateway") return structuredClone(state);
      if (name === "update_gateway") {
        if (++attempts === 1) {
          state.revision = "other-window";
          state.providers[0].queued = false;
          throw { code: "CONFLICT", message: "网关设置已变化" };
        }
        expect(args!.expectedRevision).toBe("other-window");
        state.providers[0].maxConcurrency = args!.edit.maxConcurrency;
        state.revision = "saved";
        return structuredClone(state);
      }
    },
  );
  render(<Gateway notify={() => {}} />);
  await user.click(
    await screen.findByRole("button", { name: "api.example.com 并发上限" }),
  );
  const input = screen.getByRole("spinbutton", { name: "上限（0 不限）" });
  await user.clear(input);
  await user.type(input, "12");
  await user.click(screen.getByRole("button", { name: "保存" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("网关设置已变化");
  expect(input).toHaveValue(12);
  expect(
    screen.getByRole("button", { name: "api.example.com 加入队列" }),
  ).toHaveAttribute("aria-pressed", "false");
  await user.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  expect(
    screen.getByRole("button", { name: "api.example.com 并发上限" }),
  ).toHaveTextContent("并发 0/12");
  expect(
    mock.command.mock.calls
      .filter(([name]) => name === "update_gateway")
      .map(([, args]) => args.edit.op),
  ).toEqual(["concurrencyProvider", "concurrencyProvider"]);
  expect(state.providers[0].queued).toBe(false);
});

describe("client isolation", () => {
  it("edits bounded capacity waiting for every client and preserves the draft during events", async () => {
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await screen.findByLabelText(`${state.providers[0].name} 操作`);
    await user.click(screen.getByText("故障策略", { selector: "summary" }));
    const input = screen.getByRole("spinbutton", { name: "容量错误等待 / 秒" });
    expect(input).toHaveValue(60);
    expect(input).toHaveAttribute("min", "1");
    expect(input).toHaveAttribute("max", "86400");
    await user.clear(input);
    await user.type(input, "120");
    act(() =>
      mock.listeners.get("gateway-state")!({ ...state, activeConnections: 3 }),
    );
    expect(input).toHaveValue(120);
    await user.click(screen.getByRole("button", { name: "保存参数" }));
    await waitFor(() =>
      expect(mock.command).toHaveBeenCalledWith("update_gateway", {
        clientId: "codex",
        edit: {
          op: "settings",
          settings: { ...state.settings, capacityRetrySeconds: 120 },
        },
        expectedRevision: state.revision,
      }),
    );
    await user.click(screen.getByRole("button", { name: "Claude Code" }));
    await screen.findByLabelText(`${claudeState.providers[0].name} 操作`);
    await user.click(screen.getByText("故障策略", { selector: "summary" }));
    expect(
      screen.getByRole("spinbutton", { name: "容量错误等待 / 秒" }),
    ).toHaveValue(claudeState.settings.capacityRetrySeconds);
  });
  it("queries each selected client and ignores the other client's gateway events", async () => {
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await screen.findByLabelText(`${state.providers[0].name} 操作`);
    expect(mock.command).toHaveBeenCalledWith("get_gateway", {
      clientId: "codex",
    });
    await waitFor(() =>
      expect(mock.command).toHaveBeenCalledWith("query_provider_quota", {
        clientId: "codex",
        providerId: state.providers[0].id,
        force: false,
      }),
    );
    act(() =>
      mock.listeners.get("gateway-state")!({
        ...claudeState,
        revision: "background-claude",
      }),
    );
    expect(
      screen.getByLabelText(`${state.providers[0].name} 操作`),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText(`${claudeState.providers[0].name} 操作`),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Claude Code" }));
    await screen.findByLabelText(`${claudeState.providers[0].name} 操作`);
    expect(
      screen.queryByLabelText(`${state.providers[0].name} 操作`),
    ).not.toBeInTheDocument();
    expect(mock.command).toHaveBeenCalledWith("get_gateway", {
      clientId: "claude",
    });
    await waitFor(() =>
      expect(mock.command).toHaveBeenCalledWith("query_provider_quota", {
        clientId: "claude",
        providerId: claudeState.providers[0].id,
        force: false,
      }),
    );
    expect(localStorage.getItem("vela.main.client")).toBe("claude");
    act(() =>
      mock.listeners.get("gateway-state")!({
        ...state,
        revision: "background-codex",
        providers: state.providers.map((p) => ({
          ...p,
          name: `后台 ${p.name}`,
        })),
      }),
    );
    expect(
      screen.getByLabelText(`${claudeState.providers[0].name} 操作`),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText(`后台 ${state.providers[0].name} 操作`),
    ).not.toBeInTheDocument();
    act(() =>
      mock.listeners.get("gateway-state")!({
        ...claudeState,
        revision: "foreground-claude",
        providers: claudeState.providers.map((p, index) => ({
          ...p,
          name: index ? p.name : "当前 Claude 供应商",
        })),
      }),
    );
    expect(
      screen.getByLabelText("当前 Claude 供应商 操作"),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Codex" }));
    await screen.findByLabelText(`${state.providers[0].name} 操作`);
    expect(
      mock.command.mock.calls
        .filter(([name]) => name === "get_gateway")
        .map(([, args]) => args.clientId),
    ).toEqual(["codex", "claude", "codex"]);
    expect(
      mock.command.mock.calls.some(([name]) => name === "update_gateway"),
    ).toBe(false);
  });

  it("keeps an unsaved settings draft when switching is refused and discards it only after confirmation", async () => {
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await screen.findByLabelText(`${state.providers[0].name} 操作`);
    await user.click(screen.getByText("高级设置", { selector: "summary" }));
    const port = screen.getByRole("spinbutton", { name: "共享本地端口" });
    await user.clear(port);
    await user.type(port, "23456");
    await user.click(screen.getByRole("button", { name: "Claude Code" }));
    await user.click(
      within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
        "button",
        { name: "取消" },
      ),
    );
    expect(port).toHaveValue(23456);
    expect(screen.getByRole("button", { name: "Codex" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(mock.command).not.toHaveBeenCalledWith("get_gateway", {
      clientId: "claude",
    });

    await user.click(screen.getByRole("button", { name: "Claude Code" }));
    await user.click(
      within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
        "button",
        { name: "放弃修改" },
      ),
    );
    await screen.findByLabelText(`${claudeState.providers[0].name} 操作`);
    await user.click(screen.getByText("高级设置", { selector: "summary" }));
    expect(
      screen.getByRole("spinbutton", { name: "共享本地端口" }),
    ).toHaveValue(claudeState.settings.port);
    expect(
      mock.command.mock.calls.some(([name]) => name === "update_gateway"),
    ).toBe(false);
    await user.click(screen.getByRole("button", { name: "Codex" }));
    await screen.findByLabelText(`${state.providers[0].name} 操作`);
    expect(
      screen.queryByRole("dialog", { name: "确认操作" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByText("高级设置", { selector: "summary" }));
    expect(
      screen.getByRole("spinbutton", { name: "共享本地端口" }),
    ).toHaveValue(state.settings.port);
  });

  it("restores Claude selection and saves its credentials and stream setting with the Claude revision", async () => {
    localStorage.setItem("vela.main.client", "claude");
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await screen.findByLabelText(`${claudeState.providers[0].name} 操作`);
    expect(mock.command).not.toHaveBeenCalledWith("get_gateway", {
      clientId: "codex",
    });
    await user.click(screen.getByRole("button", { name: "添加" }));
    const dialog = screen.getByRole("dialog", { name: "添加供应商" });
    expect(
      within(dialog).getByRole("checkbox", {
        name: "收到 message_stop 后立即结束响应",
      }),
    ).not.toBeChecked();
    const base = within(dialog).getByLabelText("ANTHROPIC_BASE_URL");
    const token = within(dialog).getByLabelText("ANTHROPIC_AUTH_TOKEN");
    expect(token).toHaveAttribute("type", "password");
    expect(
      within(dialog).queryByLabelText("experimental_bearer_token"),
    ).not.toBeInTheDocument();
    await user.type(base, "https://claude.fixture.invalid/deployment");
    await user.type(token, "claude-form-fixture");
    act(() =>
      mock.listeners.get("gateway-state")!({
        ...state,
        revision: "unrelated-codex-update",
      }),
    );
    expect(base).toHaveValue("https://claude.fixture.invalid/deployment");
    expect(token).toHaveValue("claude-form-fixture");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(mock.command).toHaveBeenCalledWith("update_gateway", {
      clientId: "claude",
      edit: {
        op: "saveProvider",
        id: null,
        name: "",
        baseUrl: "https://claude.fixture.invalid/deployment",
        token: "claude-form-fixture",
        endOnMessageStop: false,
        filterCountTokens: false,
        bufferCompleteResponse: false,
        endlessMode: false,
        upstreamRetrySeconds: 33,
        limitRetrySeconds: 33,
        limitRestSeconds: 8,
        normalRetrySeconds: 3,
        riskCodes: [520],
        retryIntensity: 5,
        takeCompaction: true,
        compactionModel: "",
        compactionEffort: "",
        takeAuxiliary: true,
        auxiliaryModel: "",
        auxiliaryEffort: "",
        takeWebSearch: true,
        webSearchModel: "",
        webSearchEffort: "",
      },
      expectedRevision: claudeState.revision,
    });
  });
});

it("keeps one icon-bearing testing entry per provider and removes global testing", async () => {
  render(<Gateway notify={() => {}} />);
  await screen.findByRole("button", { name: "添加" });
  const name = state.providers[0].name;
  const actions = within(screen.getByLabelText(`${name} 操作`));
  const entry = actions.getByRole("button", { name: `${name} 连接测试` });
  expect(entry.querySelector("svg")).not.toBeNull();
  expect(
    actions.queryByRole("button", { name: /模型指纹测试/ }),
  ).not.toBeInTheDocument();
});

it.each([
  [
    { profiles: [], command: null },
    "未找到 PowerShell 配置文件，agy 仍直连；创建 Microsoft.PowerShell_profile.ps1 后重新启用",
    "未找到 PowerShell 配置文件，agy 仍直连；创建 Microsoft.PowerShell_profile.ps1 后重新启用",
  ],
  [
    { profiles: [], command: "source ~/.vela/antigravity/agy.env" },
    "未能写入 shell 启动文件，可手动运行",
    "已接入，请打开新的终端运行 agy",
  ],
  [
    { profiles: ["~/.zshrc"], command: null },
    "agy 函数已写入 ~/.zshrc",
    "已接入，请打开新的终端运行 agy",
  ],
])(
  "agy reports what enabling wrote to the shell: %o",
  async (agyShell, status, notice) => {
    localStorage.setItem("vela.main.client", "agy");
    state = { ...state, clientId: "agy", running: false, agyShell: null };
    mock.command.mockImplementation(async (name) =>
      name === "get_gateway"
        ? structuredClone(state)
        : name === "start_gateway"
          ? { ...structuredClone(state), running: true, agyShell }
          : null,
    );
    const notify = vi.fn();
    const user = userEvent.setup();
    render(<Gateway notify={notify} quotaRefreshSeconds={0} />);
    expect(
      await screen.findByText("仅终端里的 agy 经过网关"),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "启用" }));
    await waitFor(() => expect(notify).toHaveBeenCalledWith(notice));
    expect(screen.getByText(status)).toBeInTheDocument();
  },
);

it.each([
  ["codex", ["API", "白名单", "端点"]],
  ["claude", ["API", "白名单", "模型映射", "端点"]],
  ["grok", ["API", "白名单", "端点"]],
  ["dsh", ["API", "模型映射", "端点"]],
  ["agy", ["API", "白名单", "端点"]],
] as const)(
  "%s keeps five operations on the row and the rest in the edit dialog's tabs",
  async (clientId, tabs) => {
    localStorage.setItem("vela.main.client", clientId);
    state.clientId = clientId;
    mock.command.mockImplementation(async (name) =>
      name === "get_gateway"
        ? structuredClone(state)
        : name === "list_provider_models"
          ? { models: [], error: null, stale: false }
          : name === "get_gateway_models"
            ? { models: [] }
            : null,
    );
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} quotaRefreshSeconds={0} />);
    const name = state.providers[0].name;
    const group = await screen.findByRole("group", { name: `${name} 操作` });
    const buttons = within(group).getAllByRole("button");
    expect(buttons.map((button) => button.getAttribute("title"))).toEqual([
      "重置熔断",
      "连接测试",
      "编辑 API",
      "创建副本",
      "删除",
    ]);
    for (const button of buttons) {
      expect(button).toHaveAccessibleName(`${name} ${button.title}`);
      expect(button.querySelector("svg")).not.toBeNull();
    }
    expect(document.querySelector(".row-menu")).toBeNull();

    await user.click(
      within(group).getByRole("button", { name: `${name} 编辑 API` }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: `编辑供应商 · ${name}`,
    });
    const nav = within(
      within(dialog).getByRole("navigation", { name: "供应商设置" }),
    );
    expect(
      nav.getAllByRole("button").map((button) => button.textContent),
    ).toEqual(tabs);
    expect(nav.getByRole("button", { name: "API" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    // Every tab opens inside the same dialog.
    for (const tab of tabs) {
      await user.click(nav.getByRole("button", { name: tab }));
      expect(nav.getByRole("button", { name: tab })).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      expect(screen.getAllByRole("dialog")).toHaveLength(1);
    }
    expect(
      within(dialog).getByRole("button", { name: "测速" }),
    ).toBeInTheDocument();
  },
);

it("offers a provider that is not saved yet the API form alone", async () => {
  const user = userEvent.setup();
  render(<Gateway notify={() => {}} />);
  await user.click(await screen.findByRole("button", { name: "添加" }));
  const dialog = await screen.findByRole("dialog", { name: "添加供应商" });
  expect(within(dialog).queryByRole("navigation")).not.toBeInTheDocument();
  expect(within(dialog).getByLabelText("base_url")).toBeInTheDocument();
});

it("asks before a tab switch drops an unsaved draft", async () => {
  const user = userEvent.setup();
  render(<Gateway notify={() => {}} />);
  const name = state.providers[0].name;
  await user.click(
    await screen.findByRole("button", { name: `${name} 编辑 API` }),
  );
  const dialog = within(
    await screen.findByRole("dialog", { name: `编辑供应商 · ${name}` }),
  );
  const tab = (label: string) =>
    within(dialog.getByRole("navigation", { name: "供应商设置" })).getByRole(
      "button",
      { name: label },
    );
  await user.type(dialog.getByRole("textbox", { name: "名称" }), "草稿");
  await user.click(tab("端点"));
  await user.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "取消" },
    ),
  );
  await waitFor(() =>
    expect(
      screen.queryByRole("dialog", { name: "确认操作" }),
    ).not.toBeInTheDocument(),
  );
  expect(tab("API")).toHaveAttribute("aria-pressed", "true");
  expect(dialog.getByRole("textbox", { name: "名称" })).toHaveValue(
    `${name}草稿`,
  );
  await user.click(tab("端点"));
  await user.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "放弃修改" },
    ),
  );
  await waitFor(() =>
    expect(tab("端点")).toHaveAttribute("aria-pressed", "true"),
  );
  expect(
    mock.command.mock.calls.some(([command]) => command === "update_gateway"),
  ).toBe(false);
});

describe("saving a draft from the tab bar", () => {
  const edits = () =>
    mock.command.mock.calls
      .filter(([name]) => name === "update_gateway")
      .map(([, args]) => args.edit);
  const choose = async (label: string) =>
    userEvent.click(
      within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
        "button",
        { name: label },
      ),
    );
  // `update` answers each edit; by default the gateway accepts it unchanged.
  const open = async (update: (edit: { op: string }) => void = () => {}) => {
    mock.command.mockImplementation(async (name: string, args) => {
      if (name === "list_provider_models")
        return { models: ["gpt-A", "gpt-B"], error: null, stale: false };
      if (name === "update_gateway") update(args.edit);
      if (name === "get_gateway" || name === "update_gateway")
        return structuredClone(state);
    });
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    const name = state.providers[0].name;
    await user.click(
      await screen.findByRole("button", { name: `${name} 编辑 API` }),
    );
    const dialog = within(
      await screen.findByRole("dialog", { name: `编辑供应商 · ${name}` }),
    );
    const tab = (label: string) =>
      within(dialog.getByRole("navigation", { name: "供应商设置" })).getByRole(
        "button",
        { name: label },
      );
    return { user, dialog, tab, name };
  };
  const closed = (name: string) =>
    waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: `编辑供应商 · ${name}` }),
      ).not.toBeInTheDocument(),
    );

  it("saves the whitelist and moves on to the tab that was picked", async () => {
    const { user, dialog, tab, name } = await open();
    await user.click(tab("白名单"));
    await user.click(await dialog.findByRole("checkbox", { name: "gpt-A" }));
    await user.click(tab("端点"));
    const confirm = await screen.findByRole("dialog", { name: "确认操作" });
    expect(
      within(confirm)
        .getAllByRole("button")
        .map((button) => button.textContent),
    ).toEqual(["", "取消", "放弃修改", "保存"]);
    await choose("保存");
    await waitFor(() =>
      expect(tab("端点")).toHaveAttribute("aria-pressed", "true"),
    );
    expect(edits()).toEqual([
      { op: "policyProvider", id: "primary", allowedModels: ["gpt-A"] },
    ]);
    expect(
      screen.getByRole("dialog", { name: `编辑供应商 · ${name}` }),
    ).toBeInTheDocument();
  });

  it("opens the next tab on the provider as the save left it", async () => {
    const { user, dialog, tab, name } = await open((edit) => {
      if (edit.op === "renameProvider")
        state.providers[0].name = (edit as { name?: string }).name!;
    });
    await user.type(dialog.getByRole("textbox", { name: "名称" }), "草稿");
    await user.click(tab("端点"));
    await choose("保存");
    const renamed = await screen.findByRole("dialog", {
      name: `编辑供应商 · ${name}草稿`,
    });
    expect(
      within(renamed).getByRole("button", { name: "端点" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(edits().map((edit) => edit.op)).toEqual([
      "renameProvider",
      "saveProvider",
    ]);
  });

  it("stays on the tab when its form rejects the draft, and a later save still closes", async () => {
    const { user, dialog, tab, name } = await open();
    await user.click(tab("白名单"));
    await dialog.findByRole("checkbox", { name: "gpt-A" });
    await user.click(
      within(dialog.getByRole("group", { name: "模型规则" })).getByRole(
        "button",
        { name: "白名单" },
      ),
    );
    await user.click(tab("端点"));
    await choose("保存");
    expect(await dialog.findByRole("alert")).toHaveTextContent(
      "白名单至少选择一个模型",
    );
    expect(tab("白名单")).toHaveAttribute("aria-pressed", "true");
    expect(edits()).toEqual([]);
    await user.click(dialog.getByRole("checkbox", { name: "gpt-A" }));
    await user.click(dialog.getByRole("button", { name: "保存" }));
    await closed(name);
    expect(edits()).toHaveLength(1);
  });

  it("stays on the tab when the save fails, and the retry still closes", async () => {
    let failing = true;
    const { user, dialog, tab, name } = await open(() => {
      if (failing) throw { code: "CONFLICT", message: "设置已变化" };
    });
    await user.click(tab("白名单"));
    await user.click(await dialog.findByRole("checkbox", { name: "gpt-A" }));
    await user.click(tab("端点"));
    await choose("保存");
    expect(await dialog.findByText("设置已变化")).toBeInTheDocument();
    expect(tab("白名单")).toHaveAttribute("aria-pressed", "true");
    failing = false;
    await user.click(dialog.getByRole("button", { name: "重试" }));
    await closed(name);
  });
});

it("opens the model mapping tab for DSH model rules on request", async () => {
  localStorage.setItem("vela.main.client", "dsh");
  state.clientId = "dsh";
  mock.command.mockImplementation(async (name) =>
    name === "get_gateway"
      ? structuredClone(state)
      : name === "list_provider_models"
        ? { models: [], error: null, stale: false }
        : null,
  );
  const { id, name } = state.providers[0];
  render(
    <Gateway
      notify={() => {}}
      focusProvider={{ id, clientId: "dsh", action: "settings", sequence: 1 }}
    />,
  );
  const dialog = await screen.findByRole("dialog", {
    name: `编辑供应商 · ${name}`,
  });
  expect(
    within(dialog).getByRole("button", { name: "模型映射" }),
  ).toHaveAttribute("aria-pressed", "true");
});

it("copies a provider from its row and asks before deleting one", async () => {
  const user = userEvent.setup();
  const notify = vi.fn();
  render(<Gateway notify={notify} />);
  const { id, name } = state.providers[0];
  const actions = within(await screen.findByLabelText(`${name} 操作`));
  await user.click(actions.getByRole("button", { name: `${name} 创建副本` }));
  await waitFor(() => expect(notify).toHaveBeenCalledWith("已创建副本"));
  expect(mock.command).toHaveBeenCalledWith("update_gateway", {
    clientId: "codex",
    edit: { op: "duplicateProvider", id },
    expectedRevision: state.revision,
  });

  const sent = () =>
    mock.command.mock.calls.filter(
      ([command, args]) =>
        command === "update_gateway" && args.edit.op === "deleteProvider",
    );
  await user.click(actions.getByRole("button", { name: `${name} 删除` }));
  const confirmation = await screen.findByRole("dialog", { name: "确认操作" });
  expect(confirmation).toHaveTextContent(`删除供应商“${name}”？`);
  await user.click(within(confirmation).getByRole("button", { name: "取消" }));
  await waitFor(() =>
    expect(
      screen.queryByRole("dialog", { name: "确认操作" }),
    ).not.toBeInTheDocument(),
  );
  expect(sent()).toHaveLength(0);

  await user.click(actions.getByRole("button", { name: `${name} 删除` }));
  await user.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "删除" },
    ),
  );
  await waitFor(() => expect(sent()).toHaveLength(1));
  expect(sent()[0][1].edit).toEqual({ op: "deleteProvider", id });
});

it("maps a Claude provider's models in the edit dialog", async () => {
  localStorage.setItem("vela.main.client", "claude");
  const user = userEvent.setup();
  const notify = vi.fn();
  const { id, name } = claudeState.providers[0];
  mock.command.mockImplementation(async (command: string) =>
    command === "list_provider_models"
      ? { models: ["upstream-opus"], error: null, stale: false }
      : command === "get_gateway_models"
        ? { models: [] }
        : command === "get_gateway" || command === "update_gateway"
          ? structuredClone(claudeState)
          : undefined,
  );
  render(<Gateway notify={notify} />);
  await user.click(
    within(await screen.findByLabelText(`${name} 操作`)).getByRole("button", {
      name: `${name} 编辑 API`,
    }),
  );
  const dialog = within(
    await screen.findByRole("dialog", { name: `编辑供应商 · ${name}` }),
  );
  await user.click(dialog.getByRole("button", { name: "模型映射" }));
  await user.click(dialog.getByRole("button", { name: "添加映射" }));
  await user.type(
    dialog.getByRole("combobox", { name: "映射 1 请求模型" }),
    "claude-opus",
  );
  const upstream = await dialog.findByRole("combobox", {
    name: "映射 1 上游模型",
  });
  await user.selectOptions(
    upstream,
    await within(upstream).findByRole("option", { name: "upstream-opus" }),
  );
  await user.click(dialog.getByRole("button", { name: "保存" }));
  await waitFor(() => expect(notify).toHaveBeenCalledWith("模型映射已保存"));
  expect(mock.command).toHaveBeenCalledWith("update_gateway", {
    clientId: "claude",
    edit: {
      op: "modelMapProvider",
      id,
      mapping: { "claude-opus": "upstream-opus" },
    },
    expectedRevision: claudeState.revision,
  });
  expect(
    screen.queryByRole("dialog", { name: `编辑供应商 · ${name}` }),
  ).not.toBeInTheDocument();
});

it("opens the API editor on request and starts it afresh for each request", async () => {
  const user = userEvent.setup();
  const handled = vi.fn();
  const { id, baseUrl } = state.providers[0];
  const focus = (sequence: number) => (
    <Gateway
      notify={() => {}}
      focusProvider={{ id, clientId: "codex", action: "edit", sequence }}
      onFocusHandled={handled}
    />
  );
  const { rerender } = render(focus(1));
  const address = () =>
    within(screen.getByRole("dialog", { name: /^编辑供应商/ })).getByRole(
      "textbox",
      { name: "base_url" },
    );
  await screen.findByRole("dialog", { name: /^编辑供应商/ });
  expect(
    within(screen.getByRole("navigation", { name: "供应商设置" })).getByRole(
      "button",
      { name: "API" },
    ),
  ).toHaveAttribute("aria-pressed", "true");
  expect(address()).toHaveValue(baseUrl);
  expect(handled).toHaveBeenCalledTimes(1);
  await user.clear(address());
  await user.type(address(), "https://draft.example.invalid/v1");
  // App confirms discarding before emitting this new navigation sequence.
  rerender(focus(2));
  await waitFor(() => expect(address()).toHaveValue(baseUrl));
  expect(
    mock.command.mock.calls.some(([name]) => name === "update_gateway"),
  ).toBe(false);
});

it.each(["codex", "claude", "grok", "dsh"] as const)(
  "%s omits the connection summary and keeps client onboarding without file exchange",
  async (clientId) => {
    localStorage.setItem("vela.main.client", clientId);
    state.clientId = clientId;
    state.running = false;
    state.grokManaged = false;
    state.current = {
      kind: "gateway",
      modelSource: null,
      warning: null,
      model: "fixture-current-model",
      address: "http://127.0.0.1:43721/v1",
      protocol: "chat_completions",
    };
    mock.command.mockImplementation(async (name) =>
      name === "get_gateway" ? structuredClone(state) : null,
    );
    const { container } = render(
      <Gateway notify={() => {}} quotaRefreshSeconds={0} />,
    );
    await screen.findByRole("button", { name: "添加" });
    expect(container.querySelector(".gateway-current")).toBeNull();
    expect(screen.queryByLabelText("当前客户端配置")).not.toBeInTheDocument();
    expect(screen.queryByText("fixture-current-model")).not.toBeInTheDocument();
    expect(screen.queryByText("fixture-source")).not.toBeInTheDocument();
    expect(
      screen.queryByText("fixture-credential-source"),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("各 profile 当前配置")).not.toBeInTheDocument();
    expect(screen.queryByText("fixture-profile-model")).not.toBeInTheDocument();
    if (clientId === "grok" || clientId === "dsh")
      expect(
        screen.getByRole("button", { name: "导入并接入" }),
      ).toBeInTheDocument();
    expect(
      screen.queryByRole("group", { name: "供应商导入导出" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /^(导入|导出)$/ }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加" })).toBeInTheDocument();
  },
);

it("keeps a gateway event that arrives before the initial read", async () => {
  let resolve!: (value: GatewayState) => void;
  mock.command.mockImplementation((name) =>
    name === "get_gateway"
      ? new Promise<GatewayState>((done) => {
          resolve = done;
        })
      : Promise.resolve(null),
  );
  render(<Gateway notify={() => {}} quotaRefreshSeconds={0} />);
  const latest = { ...state, running: true };
  await act(async () => {
    mock.listeners.get("gateway-state")!(latest);
  });
  await screen.findByRole("button", { name: "停用" });
  await act(async () => {
    resolve({ ...state, running: false });
  });
  expect(screen.getByRole("button", { name: "停用" })).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "启用" }),
  ).not.toBeInTheDocument();
});

it("refreshes on show and keeps a newer gateway event over the resumed read", async () => {
  render(<Gateway notify={() => {}} quotaRefreshSeconds={0} />);
  await screen.findByRole("button", { name: "启用" });
  await act(async () => mock.listeners.get("app-visibility")!(false));
  let resolve!: (value: GatewayState) => void;
  const original = mock.command.getMockImplementation()!;
  mock.command.mockImplementation((name, args) =>
    name === "get_gateway"
      ? new Promise<GatewayState>((done) => {
          resolve = done;
        })
      : original(name, args),
  );
  await act(async () => mock.listeners.get("app-visibility")!(true));
  await act(async () =>
    mock.listeners.get("gateway-state")!({ ...state, running: true }),
  );
  await act(async () => resolve({ ...state, running: false }));
  expect(screen.getByRole("button", { name: "停用" })).toBeInTheDocument();
});

it("shows the API tab the address an endpoint swap made primary", async () => {
  const user = userEvent.setup();
  const backup = "https://backup.example.invalid/v1";
  const { name, baseUrl } = state.providers[0];
  state.providers[0].extraBaseUrls = [backup];
  mock.command.mockImplementation(async (command: string) => {
    if (command === "update_gateway") {
      state.providers[0].baseUrl = backup;
      state.providers[0].extraBaseUrls = [baseUrl];
      state.revision = "swapped";
    }
    return command === "get_gateway" || command === "update_gateway"
      ? structuredClone(state)
      : undefined;
  });
  render(<Gateway notify={() => {}} />);
  await user.click(
    await screen.findByRole("button", { name: `${name} 编辑 API` }),
  );
  const dialog = within(
    await screen.findByRole("dialog", { name: `编辑供应商 · ${name}` }),
  );
  await user.click(dialog.getByRole("button", { name: "端点" }));
  await user.click(
    dialog.getByRole("button", { name: "辅助端点 1 设为主端点" }),
  );
  expect(await dialog.findByText("主副端点已互换")).toBeInTheDocument();
  await user.click(dialog.getByRole("button", { name: "API" }));
  expect(dialog.getByRole("textbox", { name: "base_url" })).toHaveValue(backup);
});
