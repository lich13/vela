import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import GatewayProviderDialog from "./GatewayProviderDialog";
import { command } from "./bridge";
import { claudeGatewayDemo } from "./gateway-preview";
import type { Provider } from "./types";

vi.mock("./bridge", () => ({
  command: vi.fn(),
  subscribe: vi.fn(async () => () => {}),
}));

describe("Claude provider message_stop setting", () => {
  it.each([undefined, false, true])(
    "saves an independent value from %s",
    async (initial) => {
      const user = userEvent.setup();
      const save = vi.fn().mockResolvedValue(undefined);
      const dirty = vi.fn();
      const provider =
        initial === undefined
          ? undefined
          : {
              ...claudeGatewayDemo.providers[0],
              endOnMessageStop: initial,
            };
      render(
        <GatewayProviderDialog
          clientId="claude"
          connectionMode="bearer"
          dialog={{ kind: "provider", item: provider }}
          revision="fixture-revision"
          close={() => {}}
          save={save}
          onDirtyChange={dirty}
        />,
      );
      const control = screen.getByRole("checkbox", {
        name: "收到 message_stop 后立即结束响应",
      });
      expect(control).toHaveProperty("checked", Boolean(initial));
      if (!provider) {
        await user.type(
          screen.getByLabelText("ANTHROPIC_BASE_URL"),
          "https://fixture.example.invalid",
        );
        await user.type(
          screen.getByLabelText("ANTHROPIC_AUTH_TOKEN"),
          "fixture-token",
        );
      }
      await user.click(control);
      expect(dirty).toHaveBeenLastCalledWith(true);
      await user.click(screen.getByRole("button", { name: "保存" }));
      expect(save).toHaveBeenCalledWith(
        expect.objectContaining({
          op: "saveProvider",
          id: provider?.id ?? null,
          endOnMessageStop: !initial,
        }),
        "fixture-revision",
      );
      expect(save.mock.calls[0][0]).not.toHaveProperty("searchOnly");
    },
  );
});

describe("Claude reliability controls", () => {
  it("tracks drafts, persists independent controls and retains values after a failed save", async () => {
    const user = userEvent.setup();
    const dirty = vi.fn();
    const save = vi
      .fn()
      .mockRejectedValueOnce(new Error("fixture save failed"))
      .mockResolvedValueOnce(undefined);
    const item = { ...claudeGatewayDemo.providers[0], upstreamRetrySeconds: 5 };
    render(
      <GatewayProviderDialog
        clientId="claude"
        connectionMode="bearer"
        dialog={{ kind: "provider", item }}
        revision="fixture-revision"
        close={() => {}}
        save={save}
        onDirtyChange={dirty}
      />,
    );
    expect(
      screen.queryByRole("spinbutton", { name: "风控重试 秒" }),
    ).toBeNull();
    for (const name of ["过滤 Token 计数请求", "完整响应后发送", "无尽模式"])
      await user.click(screen.getByRole("checkbox", { name }));
    const interval = screen.getByRole("spinbutton", { name: "风控重试 秒" });
    expect(interval).toHaveValue(5);
    await user.clear(interval);
    await user.type(interval, "7");
    const normal = screen.getByRole("spinbutton", { name: "普通重试 秒" });
    expect(normal).toHaveValue(3);
    await user.clear(normal);
    await user.type(normal, "4");
    expect(dirty).toHaveBeenLastCalledWith(true);
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "fixture save failed",
    );
    expect(interval).toHaveValue(7);
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(save).toHaveBeenLastCalledWith(
      expect.objectContaining({
        filterCountTokens: true,
        bufferCompleteResponse: true,
        endlessMode: true,
        upstreamRetrySeconds: 7,
        normalRetrySeconds: 4,
      }),
      null,
    );
  });
});

describe("endless mode retry settings", () => {
  const open = (
    clientId: "codex" | "claude" | "grok" | "dsh",
    save = vi.fn().mockResolvedValue(undefined),
  ) => {
    render(
      <GatewayProviderDialog
        clientId={clientId}
        connectionMode="bearer"
        dialog={{
          kind: "provider",
          item: {
            ...claudeGatewayDemo.providers[0],
            riskCodes: clientId === "claude" ? [520] : [],
          },
        }}
        revision="fixture-revision"
        close={() => {}}
        save={save}
      />,
    );
    return save;
  };

  it.each(["codex", "claude", "dsh"] as const)(
    "offers %s the three waits, the rest, the risk codes and the intensity",
    async (clientId) => {
      const user = userEvent.setup();
      const save = open(clientId);
      expect(screen.queryByRole("textbox", { name: "风控代码" })).toBeNull();
      await user.click(screen.getByRole("checkbox", { name: "无尽模式" }));
      const number = (name: string) => screen.getByRole("spinbutton", { name });
      expect(number("风控重试 秒")).toHaveValue(33);
      expect(number("限流重试 秒")).toHaveValue(33);
      const rest = number("限流静默 秒");
      expect(rest).toHaveValue(8);
      expect(rest).toHaveAttribute("min", "0");
      expect(number("普通重试 秒")).toHaveValue(3);
      const intensity = number("动态强度 0–10");
      expect(intensity).toHaveValue(5);
      expect(intensity).toHaveAttribute("min", "0");
      expect(intensity).toHaveAttribute("max", "10");
      const codes = screen.getByRole("textbox", { name: "风控代码" });
      expect(codes).toHaveValue(clientId === "claude" ? "520" : "");

      await user.clear(number("限流重试 秒"));
      await user.type(number("限流重试 秒"), "12");
      await user.clear(rest);
      await user.type(rest, "0");
      await user.clear(intensity);
      await user.type(intensity, "0");
      await user.clear(codes);
      await user.type(codes, "524 403，520, 403");
      await user.click(screen.getByRole("button", { name: "保存" }));
      expect(save).toHaveBeenCalledWith(
        expect.objectContaining({
          op: clientId === "dsh" ? "saveDshProvider" : "saveProvider",
          endlessMode: true,
          upstreamRetrySeconds: 33,
          limitRetrySeconds: 12,
          limitRestSeconds: 0,
          normalRetrySeconds: 3,
          riskCodes: [403, 520, 524],
          retryIntensity: 0,
        }),
        "fixture-revision",
      );
    },
  );

  it("refuses risk codes that are not error statuses and keeps the draft", async () => {
    const user = userEvent.setup();
    const save = open("claude");
    await user.click(screen.getByRole("checkbox", { name: "无尽模式" }));
    const codes = screen.getByRole("textbox", { name: "风控代码" });
    for (const text of ["200", "52x", "600"]) {
      await user.clear(codes);
      await user.type(codes, text);
      await user.click(screen.getByRole("button", { name: "保存" }));
      expect(screen.getByRole("alert")).toHaveTextContent(
        "风控代码应为 400–599 的状态码，最多 16 个",
      );
      expect(codes).toHaveValue(text);
    }
    expect(save).not.toHaveBeenCalled();
    // An empty list is a deliberate "none".
    await user.clear(codes);
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({ endlessMode: true, riskCodes: [] }),
      "fixture-revision",
    );
  });

  it("keeps the saved retry settings while endless mode stays off", async () => {
    const user = userEvent.setup();
    const save = open("claude");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        endlessMode: false,
        upstreamRetrySeconds: 33,
        limitRetrySeconds: 33,
        limitRestSeconds: 8,
        normalRetrySeconds: 3,
        riskCodes: [520],
        retryIntensity: 5,
      }),
      "fixture-revision",
    );
  });

  it("leaves Grok without endless mode", () => {
    open("grok");
    expect(screen.queryByRole("checkbox", { name: "无尽模式" })).toBeNull();
  });
});

describe("request takeover", () => {
  afterEach(() => vi.mocked(command).mockReset());
  const takeovers = ["接管压缩请求", "接管辅助请求"];
  const search = "接管网络搜索请求";
  const fields = [
    "takeCompaction",
    "compactionModel",
    "compactionEffort",
    "takeAuxiliary",
    "auxiliaryModel",
    "auxiliaryEffort",
  ];
  const searchFields = ["takeWebSearch", "webSearchModel", "webSearchEffort"];
  /** Opens the form on a saved provider, or on a new one without `item`. */
  const open = (
    clientId: "codex" | "claude" | "grok" | "dsh",
    item?: Partial<Provider>,
  ) => {
    const save = vi.fn().mockResolvedValue(undefined);
    render(
      <GatewayProviderDialog
        clientId={clientId}
        connectionMode="bearer"
        dialog={{
          kind: "provider",
          item: item && { ...claudeGatewayDemo.providers[0], ...item },
        }}
        revision="fixture-revision"
        close={() => {}}
        save={save}
      />,
    );
    return save;
  };

  it.each(["codex", "claude"] as const)(
    "lets a new %s provider take both kinds over and trims the model",
    async (clientId) => {
      const user = userEvent.setup();
      const save = open(clientId);
      for (const name of takeovers)
        expect(screen.getByRole("checkbox", { name })).not.toBeChecked();
      // Codex searches go to its search-only providers instead.
      expect(screen.queryAllByRole("checkbox", { name: search })).toHaveLength(
        clientId === "claude" ? 1 : 0,
      );
      expect(screen.queryByLabelText("压缩模型")).toBeNull();
      expect(screen.queryByLabelText("辅助模型")).toBeNull();

      await user.click(screen.getByRole("checkbox", { name: "接管压缩请求" }));
      const model = screen.getByRole("textbox", { name: "压缩模型" });
      expect(model).toHaveAttribute("placeholder", "留空则沿用客户端模型");
      expect(model).toHaveAttribute("maxlength", "256");
      await user.type(model, "  fixture-compact  ");
      // The effort follows the client until one is chosen, from the levels
      // the client's API has.
      const effort = screen.getByRole("combobox", { name: "压缩思考强度" });
      expect(effort).toHaveValue("");
      expect(
        within(effort)
          .getAllByRole("option")
          .map((option) => option.textContent),
      ).toEqual(
        clientId === "claude"
          ? ["跟随客户端", "low", "medium", "high", "xhigh", "max"]
          : [
              "跟随客户端",
              "none",
              "minimal",
              "low",
              "medium",
              "high",
              "xhigh",
              "max",
              "ultra",
            ],
      );
      await user.selectOptions(effort, "low");
      await user.click(screen.getByRole("checkbox", { name: "接管辅助请求" }));
      expect(screen.getByRole("textbox", { name: "辅助模型" })).toHaveValue("");
      expect(
        screen.getByRole("combobox", { name: "辅助思考强度" }),
      ).toHaveValue("");
      // A provider that is not saved yet has no model list to suggest from.
      expect(command).not.toHaveBeenCalled();

      await user.type(
        screen.getByLabelText(
          clientId === "claude" ? "ANTHROPIC_BASE_URL" : "base_url",
        ),
        "https://fixture.example.invalid",
      );
      await user.type(
        screen.getByLabelText(
          clientId === "claude"
            ? "ANTHROPIC_AUTH_TOKEN"
            : "experimental_bearer_token",
        ),
        "fixture-token",
      );
      await user.click(screen.getByRole("button", { name: "保存" }));
      expect(save).toHaveBeenCalledWith(
        expect.objectContaining({
          op: "saveProvider",
          id: null,
          takeCompaction: true,
          compactionModel: "fixture-compact",
          compactionEffort: "low",
          takeAuxiliary: true,
          auxiliaryModel: "",
          auxiliaryEffort: "",
        }),
        "fixture-revision",
      );
      if (clientId === "codex")
        for (const field of searchFields)
          expect(save.mock.calls[0][0]).not.toHaveProperty(field);
    },
  );

  it("loads a saved takeover, suggests the provider's models and clears a model with an empty value", async () => {
    const user = userEvent.setup();
    vi.mocked(command).mockResolvedValue({
      models: ["fixture-large", "fixture-small"],
    } as never);
    const save = open("codex", {
      takeCompaction: true,
      compactionModel: "fixture-compact",
      compactionEffort: "fixture-effort",
      auxiliaryModel: "fixture-auxiliary",
      auxiliaryEffort: "high",
    });
    expect(
      screen.getByRole("checkbox", { name: "接管压缩请求" }),
    ).toBeChecked();
    expect(
      screen.getByRole("checkbox", { name: "接管辅助请求" }),
    ).not.toBeChecked();
    const model = screen.getByLabelText("压缩模型");
    expect(model).toHaveValue("fixture-compact");
    expect(screen.queryByLabelText("辅助模型")).toBeNull();
    expect(screen.queryByLabelText("辅助思考强度")).toBeNull();
    // A saved effort the list does not know stays selectable.
    const effort = screen.getByRole("combobox", { name: "压缩思考强度" });
    expect(effort).toHaveValue("fixture-effort");
    expect(within(effort).getAllByRole("option")).toHaveLength(10);

    await waitFor(() => expect(model).toHaveAttribute("list"));
    expect(command).toHaveBeenCalledWith("list_provider_models", {
      clientId: "codex",
      providerId: claudeGatewayDemo.providers[0].id,
      force: false,
    });
    const suggestions = document.getElementById(model.getAttribute("list")!)!;
    expect(
      [...suggestions.querySelectorAll("option")].map((option) => option.value),
    ).toEqual(["fixture-large", "fixture-small"]);

    await user.clear(model);
    await user.selectOptions(effort, "");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        takeCompaction: true,
        compactionModel: "",
        compactionEffort: "",
        // What a takeover that stays off runs on is sent back as saved.
        takeAuxiliary: false,
        auxiliaryModel: "fixture-auxiliary",
        auxiliaryEffort: "high",
      }),
      "fixture-revision",
    );
  });

  it("hides both takeovers on a search-only Codex provider and saves them off", async () => {
    const user = userEvent.setup();
    vi.mocked(command).mockRejectedValue(new Error("fixture list failed"));
    const save = open("codex", {
      takeCompaction: true,
      compactionModel: "fixture-compact",
      takeAuxiliary: true,
    });
    await waitFor(() => expect(command).toHaveBeenCalled());
    await user.click(screen.getByRole("checkbox", { name: "仅网络搜索" }));
    // A model list that cannot be read is not reported.
    expect(screen.queryByRole("alert")).toBeNull();
    for (const name of takeovers)
      expect(screen.queryByRole("checkbox", { name })).toBeNull();
    expect(screen.queryByLabelText("压缩模型")).toBeNull();
    expect(screen.queryByLabelText("辅助模型")).toBeNull();

    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        searchOnly: true,
        takeCompaction: false,
        takeAuxiliary: false,
      }),
      "fixture-revision",
    );
  });

  it.each(["grok", "dsh"] as const)(
    "leaves %s without the compaction and auxiliary takeovers",
    async (clientId) => {
      const user = userEvent.setup();
      const save = open(clientId, {});
      for (const name of takeovers)
        expect(screen.queryByRole("checkbox", { name })).toBeNull();
      // Of the two, Grok Build alone has web searches to hand over.
      expect(screen.queryAllByRole("checkbox", { name: search })).toHaveLength(
        clientId === "grok" ? 1 : 0,
      );
      await user.click(screen.getByRole("button", { name: "保存" }));
      expect(save).toHaveBeenCalledTimes(1);
      for (const field of clientId === "grok"
        ? fields
        : [...fields, ...searchFields])
        expect(save.mock.calls[0][0]).not.toHaveProperty(field);
    },
  );

  it("offers a Claude provider the web searches beside the other two", async () => {
    const user = userEvent.setup();
    const save = open("claude", {
      webSearchModel: "fixture-search",
      webSearchEffort: "medium",
    });
    for (const name of [...takeovers, search])
      expect(screen.getByRole("checkbox", { name })).not.toBeChecked();
    expect(screen.queryByLabelText("搜索模型")).toBeNull();
    expect(screen.queryByLabelText("搜索思考强度")).toBeNull();

    // The model and effort saved with the option come back with it.
    await user.click(screen.getByRole("checkbox", { name: search }));
    const model = screen.getByRole("textbox", { name: "搜索模型" });
    expect(model).toHaveValue("fixture-search");
    expect(model).toHaveAttribute("placeholder", "留空则沿用客户端模型");
    const effort = screen.getByRole("combobox", { name: "搜索思考强度" });
    expect(effort).toHaveValue("medium");
    expect(
      within(effort)
        .getAllByRole("option")
        .map((option) => option.textContent),
    ).toEqual(["跟随客户端", "low", "medium", "high", "xhigh", "max"]);
    expect(command).toHaveBeenCalledWith("list_provider_models", {
      clientId: "claude",
      providerId: claudeGatewayDemo.providers[0].id,
      force: false,
    });

    await user.clear(model);
    await user.type(model, "  fixture-searcher  ");
    await user.selectOptions(effort, "low");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        op: "saveProvider",
        takeCompaction: false,
        takeAuxiliary: false,
        takeWebSearch: true,
        webSearchModel: "fixture-searcher",
        webSearchEffort: "low",
      }),
      "fixture-revision",
    );
  });

  it("offers a new Grok provider the web searches alone", async () => {
    const user = userEvent.setup();
    const save = open("grok");
    expect(screen.getByLabelText("API 协议")).toHaveValue("responses");
    expect(screen.getByRole("checkbox", { name: search })).not.toBeChecked();
    for (const name of [...takeovers, "无尽模式"])
      expect(screen.queryByRole("checkbox", { name })).toBeNull();

    await user.click(screen.getByRole("checkbox", { name: search }));
    await user.type(
      screen.getByRole("textbox", { name: "搜索模型" }),
      "fixture-search",
    );
    // The efforts are those of the Responses API.
    const effort = screen.getByRole("combobox", { name: "搜索思考强度" });
    expect(within(effort).getAllByRole("option")).toHaveLength(9);
    await user.selectOptions(effort, "low");
    await user.type(
      screen.getByLabelText("base_url"),
      "https://fixture.example.invalid/v1",
    );
    await user.type(screen.getByLabelText("api_key"), "fixture-token");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      {
        op: "saveGrokProvider",
        id: null,
        name: "",
        baseUrl: "https://fixture.example.invalid/v1",
        token: "fixture-token",
        protocol: "responses",
        takeWebSearch: true,
        webSearchModel: "fixture-search",
        webSearchEffort: "low",
      },
      "fixture-revision",
    );
  });

  it("hides the web searches on a Grok provider of another protocol and saves them off", async () => {
    const user = userEvent.setup();
    const save = open("grok", {
      protocol: "responses",
      takeWebSearch: true,
      webSearchModel: "fixture-search",
      webSearchEffort: "low",
    });
    expect(screen.getByRole("checkbox", { name: search })).toBeChecked();
    expect(screen.getByLabelText("搜索模型")).toHaveValue("fixture-search");
    expect(screen.getByLabelText("搜索思考强度")).toHaveValue("low");

    // Grok Build sends its searches as Responses requests only.
    await user.selectOptions(
      screen.getByLabelText("API 协议"),
      "chat_completions",
    );
    expect(screen.queryByRole("checkbox", { name: search })).toBeNull();
    expect(screen.queryByLabelText("搜索模型")).toBeNull();
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        op: "saveGrokProvider",
        protocol: "chat_completions",
        takeWebSearch: false,
        // What it ran on is kept for when the protocol is switched back.
        webSearchModel: "fixture-search",
        webSearchEffort: "low",
      }),
      "fixture-revision",
    );
  });
});
