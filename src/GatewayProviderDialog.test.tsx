import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import GatewayProviderDialog from "./GatewayProviderDialog";
import { command } from "./bridge";
import { claudeGatewayDemo } from "./gateway-preview";
import type { Provider } from "./types";

vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);

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
          update={() => {}}
          notify={() => {}}
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
        update={() => {}}
        notify={() => {}}
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
        update={() => {}}
        notify={() => {}}
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
        update={() => {}}
        notify={() => {}}
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
    "has a new %s provider take every kind over and trims the model",
    async (clientId) => {
      const user = userEvent.setup();
      const save = open(clientId);
      // A new provider starts with every takeover on.
      for (const name of [...takeovers, search])
        expect(screen.getByRole("checkbox", { name })).toBeChecked();
      expect(screen.getByRole("textbox", { name: "搜索模型" })).toHaveValue("");

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
          takeWebSearch: true,
          webSearchModel: "",
          webSearchEffort: "",
        }),
        "fixture-revision",
      );
      expect(save.mock.calls[0][0]).not.toHaveProperty("searchOnly");
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

  it("offers a saved Codex provider the web searches without switching anything on", async () => {
    const user = userEvent.setup();
    vi.mocked(command).mockRejectedValue(new Error("fixture list failed"));
    const save = open("codex", {
      takeCompaction: true,
      compactionModel: "fixture-compact",
    });
    await waitFor(() => expect(command).toHaveBeenCalled());
    // A model list that cannot be read is not reported.
    expect(screen.queryByRole("alert")).toBeNull();
    // A saved provider keeps what it has: only a new one starts all on.
    expect(
      screen.getByRole("checkbox", { name: "接管压缩请求" }),
    ).toBeChecked();
    for (const name of ["接管辅助请求", search])
      expect(screen.getByRole("checkbox", { name })).not.toBeChecked();
    expect(screen.queryByLabelText("搜索模型")).toBeNull();

    await user.click(screen.getByRole("checkbox", { name: search }));
    await user.type(
      screen.getByRole("textbox", { name: "搜索模型" }),
      "  fixture-search  ",
    );
    // The efforts are those of the Responses API.
    const effort = screen.getByRole("combobox", { name: "搜索思考强度" });
    expect(within(effort).getAllByRole("option")).toHaveLength(9);
    await user.selectOptions(effort, "low");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        op: "saveProvider",
        takeCompaction: true,
        compactionModel: "fixture-compact",
        takeAuxiliary: false,
        takeWebSearch: true,
        webSearchModel: "fixture-search",
        webSearchEffort: "low",
      }),
      "fixture-revision",
    );
    expect(save.mock.calls[0][0]).not.toHaveProperty("searchOnly");
  });

  it.each(["grok", "dsh"] as const)(
    "leaves %s without the compaction and auxiliary takeovers",
    async (clientId) => {
      const user = userEvent.setup();
      const save = open(clientId, {});
      for (const name of takeovers)
        expect(screen.queryByRole("checkbox", { name })).toBeNull();
      // Of the two, Grok Build alone has web searches to hand over, and a
      // saved provider's stay as they are.
      expect(screen.queryAllByRole("checkbox", { name: search })).toHaveLength(
        clientId === "grok" ? 1 : 0,
      );
      if (clientId === "grok")
        expect(
          screen.getByRole("checkbox", { name: search }),
        ).not.toBeChecked();
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
    // A new provider starts with the takeover on.
    expect(screen.getByRole("checkbox", { name: search })).toBeChecked();
    for (const name of [...takeovers, "无尽模式"])
      expect(screen.queryByRole("checkbox", { name })).toBeNull();

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

describe("risk codes met on the same host", () => {
  afterEach(() => vi.mocked(command).mockReset());

  it("ticks the met statuses and keeps the listed and typed ones", async () => {
    const user = userEvent.setup();
    const save = vi.fn().mockResolvedValue(undefined);
    const dirty = vi.fn();
    vi.mocked(command).mockImplementation((async (name: string) =>
      name === "get_provider_seen_statuses"
        ? [
            { status: 429, count: 12, basis: "rpm", lastAt: 2 },
            { status: 503, count: 3, basis: null, lastAt: 1 },
          ]
        : undefined) as never);
    const item = {
      ...claudeGatewayDemo.providers[0],
      endlessMode: true,
      riskCodes: [520],
    };
    render(
      <GatewayProviderDialog
        update={() => {}}
        notify={() => {}}
        clientId="claude"
        connectionMode="bearer"
        dialog={{ kind: "provider", item }}
        revision="fixture-revision"
        close={() => {}}
        save={save}
        onDirtyChange={dirty}
      />,
    );
    const met = await screen.findByRole("checkbox", {
      name: "429 · 12 次 · 上游 RPM 限制",
    });
    expect(command).toHaveBeenCalledWith("get_provider_seen_statuses", {
      clientId: "claude",
      providerId: item.id,
    });
    expect(met).not.toBeChecked();
    expect(
      screen.getByRole("checkbox", { name: "520 · 未遇到" }),
    ).toBeChecked();
    const other = screen.getByRole("textbox", { name: "其他代码" });
    expect(other).toHaveValue("");
    // Moving the listed code into the list is not an edit.
    expect(dirty).toHaveBeenLastCalledWith(false);

    await user.click(met);
    await user.click(screen.getByRole("checkbox", { name: "503 · 3 次" }));
    await user.click(screen.getByRole("checkbox", { name: "503 · 3 次" }));
    await user.type(other, "524");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({ riskCodes: [429, 520, 524] }),
      "fixture-revision",
    );
  });

  it("leaves the typed field alone when nothing was met", async () => {
    vi.mocked(command).mockResolvedValue([] as never);
    render(
      <GatewayProviderDialog
        update={() => {}}
        notify={() => {}}
        clientId="claude"
        connectionMode="bearer"
        dialog={{
          kind: "provider",
          item: {
            ...claudeGatewayDemo.providers[0],
            endlessMode: true,
            riskCodes: [520],
          },
        }}
        revision="fixture-revision"
        close={() => {}}
        save={vi.fn()}
      />,
    );
    expect(screen.getByText("暂无已遇到的报错")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "风控代码" })).toHaveValue(
      "520",
    );
  });
});

describe("Vertex AI access", () => {
  const open = (clientId: "claude" | "agy", item?: Provider) => {
    const save = vi.fn().mockResolvedValue(undefined);
    render(
      <GatewayProviderDialog
        update={() => {}}
        notify={() => {}}
        clientId={clientId}
        connectionMode="bearer"
        dialog={{ kind: "provider", item }}
        revision="fixture-revision"
        close={() => {}}
        save={save}
      />,
    );
    return save;
  };

  it("builds the address from the picked region and project", async () => {
    const user = userEvent.setup();
    const save = open("claude");
    await user.click(screen.getByRole("button", { name: "Vertex AI" }));
    const address = screen.getByLabelText("ANTHROPIC_BASE_URL");
    expect(address).toHaveAttribute("readonly");
    expect(address).toHaveValue(
      "https://aiplatform.googleapis.com/v1/publishers/anthropic",
    );
    await user.type(screen.getByLabelText("项目 ID"), "fixture-project");
    expect(address).toHaveValue(
      "https://aiplatform.googleapis.com/v1/projects/fixture-project/locations/global/publishers/anthropic",
    );
    await user.selectOptions(screen.getByLabelText("区域"), "us-east5");
    await user.type(screen.getByLabelText("Google API Key"), "fixture-key");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(save).toHaveBeenCalledWith(
      expect.objectContaining({
        baseUrl:
          "https://us-east5-aiplatform.googleapis.com/v1/projects/fixture-project/locations/us-east5/publishers/anthropic",
        token: "fixture-key",
      }),
      "fixture-revision",
    );
  });

  it("reads a saved Vertex AI address back into its region and project", () => {
    open("agy", {
      ...claudeGatewayDemo.providers[0],
      baseUrl:
        "https://europe-west4-aiplatform.googleapis.com/v1/projects/fixture-project/locations/europe-west4/publishers/google",
    });
    expect(screen.getByRole("button", { name: "Vertex AI" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByLabelText("区域")).toHaveValue("europe-west4");
    expect(screen.getByLabelText("项目 ID")).toHaveValue("fixture-project");
  });

  it("keeps any other address a plain endpoint", () => {
    open("claude", {
      ...claudeGatewayDemo.providers[0],
      baseUrl:
        "https://us-east5-aiplatform.googleapis.com/v1/projects/fixture-project/locations/global/publishers/anthropic",
    });
    expect(screen.getByRole("button", { name: "普通端点" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.queryByLabelText("区域")).not.toBeInTheDocument();
  });
});
