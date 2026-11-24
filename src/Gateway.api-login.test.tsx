import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GatewayState } from "./types";

import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).unfrozenBridge);

import Gateway from "./Gateway";
import { claudeGatewayDemo, gatewayDemo } from "./gateway-preview";

let codexState: GatewayState;
let claudeState: GatewayState;

function stateFor(clientId: unknown) {
  return clientId === "claude" ? claudeState : codexState;
}

beforeEach(() => {
  localStorage.clear();
  codexState = { ...structuredClone(gatewayDemo), connectionMode: "bearer" };
  claudeState = structuredClone(claudeGatewayDemo);
  mock.command.mockReset();
  mock.listeners.clear();
  mock.command.mockImplementation(
    async (name: string, args: Record<string, unknown> = {}) => {
      const current = stateFor(args.clientId);
      if (name === "get_gateway") return structuredClone(current);
      if (name === "query_provider_quota") return undefined;
      if (name === "update_gateway") {
        const edit = args.edit as {
          op?: string;
          mode?: "bearer" | "apiKey";
          id?: string;
          takeWebSearch?: boolean;
        };
        if (edit.op === "connection" && edit.mode) {
          current.connectionMode = edit.mode;
          current.revision = `saved-${current.clientId}`;
        }
        if (edit.op === "saveProvider" && edit.takeWebSearch !== undefined) {
          const provider = current.providers.find((p) => p.id === edit.id);
          if (provider) provider.takeWebSearch = edit.takeWebSearch;
        }
        return structuredClone(current);
      }
      return undefined;
    },
  );
});

async function openAdvanced(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByRole("button", { name: "添加" });
  await user.click(screen.getByText("高级设置", { selector: "summary" }));
}

async function openProviderEditor(
  user: ReturnType<typeof userEvent.setup>,
  action: "add" | "edit",
) {
  if (action === "add") {
    await user.click(screen.getByRole("button", { name: "添加" }));
    return screen.getByRole("dialog", { name: "添加供应商" });
  }
  const provider = codexState.providers[0];
  await user.click(
    within(screen.getByLabelText(`${provider.name} 操作`)).getByRole("button", {
      name: `${provider.name} 编辑 API`,
    }),
  );
  return screen.getByRole("dialog", { name: /^编辑供应商/ });
}

describe("Codex API login gateway controls", () => {
  it.each(["preserve", "replace", "clear"] as const)(
    "keeps a configured account token masked and supports %s on save",
    async (action) => {
      codexState.providers[0].hasQuotaAccessToken = true;
      const user = userEvent.setup();
      render(<Gateway notify={() => {}} />);
      await screen.findByRole("button", { name: "添加" });
      const dialog = await openProviderEditor(user, "edit");
      const token = within(dialog).getByLabelText(/账号访问令牌/);
      expect(token).toHaveAttribute("type", "password");
      expect(token).toHaveValue("");
      expect(token).toHaveAttribute("placeholder", "已配置，留空保留");
      if (action === "replace")
        await user.type(token, "fixture-account-access");
      if (action === "clear")
        await user.click(within(dialog).getByRole("button", { name: "清除" }));
      await user.click(within(dialog).getByRole("button", { name: "保存" }));
      await waitFor(() =>
        expect(mock.command).toHaveBeenCalledWith(
          "update_gateway",
          expect.anything(),
        ),
      );
      const edit = mock.command.mock.calls.find(
        ([name]) => name === "update_gateway",
      )![1].edit;
      if (action === "preserve")
        expect(edit).not.toHaveProperty("quotaAccessToken");
      else
        expect(edit.quotaAccessToken).toBe(
          action === "clear" ? "" : "fixture-account-access",
        );
      expect(
        mock.command.mock.calls.some(
          ([name]) => name === "reveal_provider_token",
        ),
      ).toBe(false);
    },
  );

  it.each(["bearer", "apiKey"] as const)(
    "preserves hidden %s connection settings",
    async (connectionMode) => {
      codexState.connectionMode = connectionMode;
      const user = userEvent.setup();
      render(<Gateway notify={() => {}} />);
      await openAdvanced(user);
      expect(
        screen.queryByRole("combobox", { name: "Codex 连接方式" }),
      ).not.toBeInTheDocument();
      expect(codexState.connectionMode).toBe(connectionMode);
      expect(
        mock.command.mock.calls.some(([name]) => name === "update_gateway"),
      ).toBe(false);
    },
  );

  it("hides Codex connection mode from Claude", async () => {
    localStorage.setItem("vela.main.client", "claude");
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await screen.findByRole("button", { name: "添加" });
    await user.click(screen.getByText("高级设置", { selector: "summary" }));

    expect(
      screen.queryByRole("combobox", { name: "Codex 连接方式" }),
    ).not.toBeInTheDocument();
  });

  it.each([
    ["bearer", "experimental_bearer_token"],
    ["apiKey", "OPENAI_API_KEY"],
  ] as const)(
    "uses %s credential labels for adding and editing suppliers",
    async (connectionMode, label) => {
      codexState.connectionMode = connectionMode;
      const user = userEvent.setup();
      render(<Gateway notify={() => {}} />);
      await screen.findByRole("button", { name: "添加" });

      const addDialog = await openProviderEditor(user, "add");
      // A new provider takes every kind over; a saved one keeps its own.
      for (const name of ["接管压缩请求", "接管辅助请求", "接管网络搜索请求"])
        expect(within(addDialog).getByRole("checkbox", { name })).toBeChecked();
      expect(within(addDialog).getByLabelText(label)).toHaveAttribute(
        "type",
        "password",
      );
      expect(
        within(addDialog).queryByLabelText(
          connectionMode === "apiKey"
            ? "experimental_bearer_token"
            : "OPENAI_API_KEY",
        ),
      ).not.toBeInTheDocument();
      await user.click(within(addDialog).getByRole("button", { name: "取消" }));

      const editDialog = await openProviderEditor(user, "edit");
      for (const name of ["接管压缩请求", "接管辅助请求", "接管网络搜索请求"])
        expect(
          within(editDialog).getByRole("checkbox", { name }),
        ).not.toBeChecked();
      expect(within(editDialog).getByLabelText(label)).toHaveAttribute(
        "type",
        "password",
      );
      expect(
        within(editDialog).queryByLabelText(
          connectionMode === "apiKey"
            ? "experimental_bearer_token"
            : "OPENAI_API_KEY",
        ),
      ).not.toBeInTheDocument();
    },
  );

  it("keeps the API supplier draft and authoritative state unchanged after a failed save", async () => {
    codexState.connectionMode = "apiKey";
    const original = structuredClone(codexState);
    mock.command.mockImplementation(
      async (name: string, args: Record<string, unknown> = {}) => {
        if (name === "get_gateway") return structuredClone(codexState);
        if (name === "query_provider_quota") return undefined;
        if (name === "update_gateway") {
          const edit = args.edit as { op?: string };
          if (edit.op === "saveProvider") {
            throw { code: "WRITE", message: "供应商保存失败" };
          }
          return structuredClone(codexState);
        }
        return undefined;
      },
    );
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await screen.findByRole("button", { name: "添加" });
    const dialog = await openProviderEditor(user, "add");
    await user.type(
      within(dialog).getByLabelText("base_url"),
      "https://api.fixture.invalid/v1",
    );
    await user.type(
      within(dialog).getByLabelText("OPENAI_API_KEY"),
      "fixture-api-key",
    );
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "供应商保存失败",
    );
    expect(within(dialog).getByLabelText("base_url")).toHaveValue(
      "https://api.fixture.invalid/v1",
    );
    expect(within(dialog).getByLabelText("OPENAI_API_KEY")).toHaveValue(
      "fixture-api-key",
    );
    expect(codexState).toEqual(original);
    expect(mock.command).toHaveBeenCalledWith("update_gateway", {
      clientId: "codex",
      edit: {
        op: "saveProvider",
        bufferCompleteResponse: false,
        id: null,
        name: "",
        baseUrl: "https://api.fixture.invalid/v1",
        token: "fixture-api-key",
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
      expectedRevision: original.revision,
    });
  });

  it("keeps a supplier draft when a background gateway refresh arrives", async () => {
    codexState.connectionMode = "apiKey";
    const user = userEvent.setup();
    render(<Gateway notify={() => {}} />);
    await screen.findByRole("button", { name: "添加" });
    const dialog = await openProviderEditor(user, "add");
    await user.type(
      within(dialog).getByLabelText("base_url"),
      "https://draft.fixture.invalid/v1",
    );
    await user.type(
      within(dialog).getByLabelText("OPENAI_API_KEY"),
      "draft-api-key",
    );

    act(() =>
      mock.listeners.get("gateway-state")!({
        ...codexState,
        revision: "background-refresh",
        activeConnections: 2,
      }),
    );

    expect(within(dialog).getByLabelText("base_url")).toHaveValue(
      "https://draft.fixture.invalid/v1",
    );
    expect(within(dialog).getByLabelText("OPENAI_API_KEY")).toHaveValue(
      "draft-api-key",
    );
  });
});

it("edits search policy only inside the API form and discards a cancelled draft", async () => {
  const user = userEvent.setup();
  render(<Gateway notify={() => {}} />);
  await screen.findByRole("button", { name: "添加" });
  expect(
    screen.queryByRole("button", { name: /接管网络搜索请求/ }),
  ).not.toBeInTheDocument();
  let dialog = await openProviderEditor(user, "edit");
  await user.click(
    within(dialog).getByRole("checkbox", { name: "接管网络搜索请求" }),
  );
  expect(
    mock.command.mock.calls.some(([name]) => name === "update_gateway"),
  ).toBe(false);
  await user.click(within(dialog).getByRole("button", { name: "取消" }));
  await user.click(
    within(await screen.findByRole("dialog", { name: "确认操作" })).getByRole(
      "button",
      { name: "放弃修改" },
    ),
  );
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  dialog = await openProviderEditor(user, "edit");
  expect(
    within(dialog).getByRole("checkbox", { name: "接管网络搜索请求" }),
  ).not.toBeChecked();
  await user.click(
    within(dialog).getByRole("checkbox", { name: "接管网络搜索请求" }),
  );
  await user.clear(within(dialog).getByLabelText("base_url"));
  await user.type(
    within(dialog).getByLabelText("base_url"),
    "https://changed.example.invalid/v1",
  );
  await user.click(within(dialog).getByRole("button", { name: "保存" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  const writes = mock.command.mock.calls.filter(
    ([name]) => name === "update_gateway",
  );
  expect(writes).toHaveLength(1);
  expect(writes[0][1]).toMatchObject({
    edit: {
      op: "saveProvider",
      takeWebSearch: true,
      baseUrl: "https://changed.example.invalid/v1",
      token: "",
    },
  });
  expect(codexState.providers[0].takeWebSearch).toBe(true);
});

it("retains the search policy draft through a save conflict and retries with the current revision", async () => {
  const user = userEvent.setup();
  const original = mock.command.getMockImplementation()!;
  let conflict = true;
  mock.command.mockImplementation(async (name, args) => {
    if (name === "update_gateway" && conflict) {
      conflict = false;
      codexState.revision = "fixture-new-revision";
      throw { code: "CONFLICT", message: "配置已变化" };
    }
    return original(name, args);
  });
  render(<Gateway notify={() => {}} />);
  await screen.findByRole("button", { name: "添加" });
  const dialog = await openProviderEditor(user, "edit");
  const initialRevision = codexState.revision;
  await user.click(
    within(dialog).getByRole("checkbox", { name: "接管网络搜索请求" }),
  );
  await user.click(within(dialog).getByRole("button", { name: "保存" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "配置已变化",
  );
  expect(
    within(dialog).getByRole("checkbox", { name: "接管网络搜索请求" }),
  ).toBeChecked();
  expect(codexState.providers[0].takeWebSearch).not.toBe(true);
  await user.click(within(dialog).getByRole("button", { name: "重试" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  const writes = mock.command.mock.calls.filter(
    ([name]) => name === "update_gateway",
  );
  expect(writes[0][1]).toMatchObject({
    expectedRevision: initialRevision,
    edit: { takeWebSearch: true },
  });
  expect(writes[1][1]).toMatchObject({
    expectedRevision: "fixture-new-revision",
    edit: { takeWebSearch: true },
  });
});
