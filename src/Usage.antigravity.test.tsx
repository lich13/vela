import UsageAntigravity from "./UsageAntigravity";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { InstanceState } from "./bridge-contracts";
import { setInstanceState } from "./instance";
import type {
  AntigravityUsageStats,
  Dashboard,
  UsageRecord,
  UsageSettings,
  UsageState,
} from "./usage-types";

const mock = vi.hoisted(() => ({
  command: vi.fn(),
  listeners: new Map<string, Set<(value: unknown) => void>>(),
}));

vi.mock("./bridge", () => ({
  preview: false,
  command: mock.command,
  subscribe: vi.fn(async (name: string, listener: (value: unknown) => void) => {
    const listeners = mock.listeners.get(name) ?? new Set();
    listeners.add(listener);
    mock.listeners.set(name, listeners);
    return () => listeners.delete(listener);
  }),
}));

vi.mock("./confirmation", () => ({ confirmAction: vi.fn(async () => true) }));

import Usage from "./Usage";
import UsageLogTable from "./UsageLogTable";
import Detail from "./UsageDetail";
import Sources from "./UsageSources";

const settings: UsageSettings = {
  recording: true,
  autoSync: false,
  refreshSeconds: 0,
  multiplier: "1",
  pricingModel: "response",
};

function record(): UsageRecord {
  return {
    id: "fixture-request",
    client: "antigravity",
    source: "antigravity",
    startedAt: 1767225600000,
    finishedAt: 1767225602000,
    completed: true,
    sessionId: null,
    duplicateOf: null,
    deduplication: "",
    attempts: [
      {
        id: "fixture-attempt",
        provider: null,
        requestedModel: null,
        responseModel: "fixture-response",
        modelDisplayName: "Fixture Model",
        reasoningEffort: "high",
        apiProvider: "API_PROVIDER_FIXTURE",
        pricingModel: "fixture-response",
        responseId: null,
        status: null,
        outcome: "success",
        startedAt: 1767225600000,
        durationMs: 2000,
        firstTokenMs: 250,
        generationMs: 1750,
        stream: true,
        transport: "local_api",
        tokens: {
          input: 100,
          output: 20,
          cacheRead: 40,
          cacheWrite: null,
          reasoning: 12,
        },
        price: null,
        serviceTier: null,
        availability: { duration: "reported", first_token: "reported" },
      },
    ],
  };
}

it("shows the display model and measured app performance without a fabricated HTTP status", () => {
  const r = record();
  r.attempts[0].tokens.output = 400;
  render(
    <UsageLogTable
      rows={[r]}
      settings={settings}
      name={() => "本机会话"}
      open={vi.fn()}
      busy={false}
    />,
  );
  const table = screen.getByRole("table", { name: "请求记录" });
  expect(within(table).getByText("Fixture Model")).toBeInTheDocument();
  expect(within(table).getByText("思考 High")).toBeInTheDocument();
  expect(within(table).getByText("0.25s")).toBeInTheDocument();
  expect(within(table).getByText("2.00s")).toBeInTheDocument();
  expect(within(table).getByText("200.0")).toBeInTheDocument();
  expect(within(table).getByText("未定价")).toBeInTheDocument();
  expect(within(table).queryByText("HTTP 200")).not.toBeInTheDocument();
});

it("leaves missing time and speed empty even in legacy app rows", () => {
  const r = record();
  const a = r.attempts[0];
  a.durationMs = 0;
  a.firstTokenMs = null;
  a.generationMs = null;
  a.availability = {};
  a.parseReasons = ["attempt_time_unreported"];
  render(
    <UsageLogTable
      rows={[r]}
      settings={settings}
      name={() => "本机会话"}
      open={vi.fn()}
      busy={false}
    />,
  );
  expect(screen.queryByText("0.00s")).not.toBeInTheDocument();
  expect(screen.queryByText("0.0")).not.toBeInTheDocument();
});

it("shows real response identity, service, request outcome and separate generation speed in details", () => {
  const r = record();
  r.attempts[0].outcome = "failed";
  r.attempts[0].tokens.output = 400;
  r.completed = false;
  render(<Detail record={r} name={() => "本机会话"} onClose={vi.fn()} />);
  expect(screen.getByText("请求失败")).toBeInTheDocument();
  expect(screen.getByText("API_PROVIDER_FIXTURE")).toBeInTheDocument();
  expect(screen.getByText("0.250s")).toBeInTheDocument();
  expect(screen.getByText("2.000s")).toBeInTheDocument();
  expect(screen.getByText("1.750s")).toBeInTheDocument();
  // 400 output tokens over 2 s; the 388 left after thinking over 1.75 s.
  expect(screen.getByText("200.0 tok/s")).toBeInTheDocument();
  expect(screen.getByText("221.7 tok/s")).toBeInTheDocument();
});

it("旧 agy 记录与 CLI 会话记录都显示为 Antigravity，来源只在详情里区分", () => {
  // A gateway row an older version stored under the CLI's own client id.
  const legacy = record();
  legacy.id = "fixture-legacy";
  legacy.client = "agy";
  legacy.source = "proxy";
  legacy.attempts[0].provider = "fixture-provider";
  legacy.attempts[0].status = 200;
  legacy.attempts[0].transport = "http";
  // A row read from a CLI session file.
  const session = record();
  session.id = "fixture-cli-session";
  session.attempts[0].transport = "cli_file";
  session.attempts[0].stream = false;
  session.attempts[0].firstTokenMs = null;

  const list = render(
    <UsageLogTable
      rows={[legacy, session]}
      settings={settings}
      name={(id) => id ?? "本机会话"}
      open={vi.fn()}
      busy={false}
    />,
  );
  const table = screen.getByRole("table", { name: "请求记录" });
  expect(within(table).getAllByText("Antigravity")).toHaveLength(2);
  expect(
    within(table).queryByText(/Antigravity (app|CLI)/),
  ).not.toBeInTheDocument();
  list.unmount();

  const field = (label: string) =>
    screen.getByText(label, { selector: "dt" }).nextElementSibling;
  const old = render(
    <Detail record={legacy} name={(id) => id ?? ""} onClose={vi.fn()} />,
  );
  expect(field("客户端")).toHaveTextContent(/^Antigravity$/);
  expect(field("来源")).toHaveTextContent(/^网关$/);
  old.unmount();

  render(<Detail record={session} name={() => "本机会话"} onClose={vi.fn()} />);
  expect(field("客户端")).toHaveTextContent(/^Antigravity$/);
  expect(field("来源")).toHaveTextContent(/^Antigravity 会话$/);
  expect(field("传输")).toHaveTextContent(/^CLI 会话文件$/);
  expect(field("首字")?.querySelector("span")).toHaveAttribute(
    "title",
    "CLI 未记录",
  );
});

it("runs explicit app history repair through the existing synchronization command", async () => {
  const state = {
    settings,
    syncing: false,
    reports: {},
    error: null,
  } as UsageState;
  mock.command.mockResolvedValue(state);
  const changed = vi.fn();
  render(
    <Sources
      state={state}
      counts={{}}
      onClose={vi.fn()}
      onChange={changed}
    />,
  );
  await userEvent.click(
    screen.getByRole("button", { name: "补全 Antigravity 历史" }),
  );
  expect(mock.command).toHaveBeenCalledWith("sync_usage", {
    rebuild: "antigravity",
  });
  expect(changed).toHaveBeenCalledWith(state);
});

function buildStats(
  overrides: Partial<AntigravityUsageStats> = {},
): AntigravityUsageStats {
  return {
    requests: 15,
    costUsd: "1.2450",
    unpricedRequests: 0,
    tokenMix: {
      input: 45_000,
      cacheRead: 180_000,
      cacheWrite: 5_000,
      output: 30_000,
      visibleOutput: 13_800,
      reasoning: 16_200,
      total: 260_000,
      thinkingSharePercent: 54.0,
      cacheHitPercent: 78.3,
      cacheReadEligible: 180_000,
      cacheInputEligible: 230_000,
      cacheSavingsUsd: "0.4860",
    },
    quotaError: null,
    accounts: [
      {
        id: "ag-oauth-1",
        name: "Fixture OAuth",
        total: {
          period: null,
          requests: 15,
          tokens: 260000,
          reasoningTokens: 16200,
          costUsd: "1.2450",
          unpricedRequests: 0,
          costPerHour: "0",
        },
        tracks: [
          {
            id: "gemini",
            label: "Gemini",
            burst5h: {
              period: {
                start: 1_760_000_000_000 - 5 * 3_600_000,
                end: 1_760_000_000_000,
                remainingPercent: 74.5,
              },
              remainingPercent: 74.5,
              requests: 6,
              tokens: 110_000,
              reasoningTokens: 7_200,
              costUsd: "0.4200",
              unpricedRequests: 0,
              costPerHour: "0.0840",
            },
            weekly7d: {
              period: {
                start: 1_760_000_000_000 - 168 * 3_600_000,
                end: 1_760_000_000_000,
                remainingPercent: 86.0,
              },
              remainingPercent: 86.0,
              requests: 10,
              tokens: 175_000,
              reasoningTokens: 10_400,
              costUsd: "0.6850",
              unpricedRequests: 0,
              costPerHour: "0.0041",
            },
          },
          {
            id: "claude_gpt",
            label: "Claude / GPT",
            burst5h: {
              period: {
                start: 1_760_000_000_000 - 5 * 3_600_000,
                end: 1_760_000_000_000,
                remainingPercent: 42.0,
              },
              remainingPercent: 42.0,
              requests: 3,
              tokens: 52_000,
              reasoningTokens: 3_600,
              costUsd: "0.3500",
              unpricedRequests: 0,
              costPerHour: "0.0700",
            },
            weekly7d: {
              period: {
                start: 1_760_000_000_000 - 168 * 3_600_000,
                end: 1_760_000_000_000,
                remainingPercent: 68.5,
              },
              remainingPercent: 68.5,
              requests: 5,
              tokens: 85_000,
              reasoningTokens: 5_800,
              costUsd: "0.5600",
              unpricedRequests: 0,
              costPerHour: "0.0033",
            },
          },
        ],
      },
    ],
    providers: [
      {
        id: "ag-oauth-1",
        requests: 10,
        tokens: 175_000,
        reasoningTokens: 10_400,
        costUsd: "0.6850",
        unpricedRequests: 0,
        sharePercent: 67.3,
      },
      {
        id: "ag-oauth-2",
        requests: 5,
        tokens: 85_000,
        reasoningTokens: 5_800,
        costUsd: "0.5600",
        unpricedRequests: 0,
        sharePercent: 32.7,
      },
    ],
    families: [
      {
        family: "gemini",
        label: "Gemini",
        requests: 10,
        tokens: 175_000,
        reasoningTokens: 10_400,
        costUsd: "0.6850",
        sharePercent: 67.3,
      },
      {
        family: "claude",
        label: "Claude",
        requests: 5,
        tokens: 85_000,
        reasoningTokens: 5_800,
        costUsd: "0.5600",
        sharePercent: 32.7,
      },
    ],
    ...overrides,
  };
}

type QuotaAnswer = { changed: boolean; error: string | null };
const instances = (
  active: "local" | "m1",
  unsupported: string[] = [],
): InstanceState => ({
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
      : { status: "connected", version: "0.42.0", error: null, unsupported },
});

describe("Usage Antigravity statistics tab", () => {
  let statsPayload: AntigravityUsageStats;
  /** What the next questions for the quota are answered with, in turn. */
  let quotaAnswers: (QuotaAnswer | Promise<QuotaAnswer>)[];
  const calls = (name: string) =>
    mock.command.mock.calls.filter(([called]) => called === name);
  const asking = () => {
    let answer!: (value: QuotaAnswer) => void;
    quotaAnswers.push(new Promise<QuotaAnswer>((done) => (answer = done)));
    return answer;
  };
  afterEach(() => {
    vi.useRealTimers();
    act(() => setInstanceState(instances("local")));
  });

  beforeEach(() => {
    mock.command.mockReset();
    mock.listeners.clear();
    statsPayload = buildStats();
    quotaAnswers = [];
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "visible",
    });

    const dashboard: Dashboard = {
      totals: {
        requests: 15,
        success: 15,
        statusKnown: 0,
        sessions: 2,
        tokens: {
          input: 45_000,
          output: 30_000,
          cacheRead: 180_000,
          cacheWrite: 5_000,
        },
        cost: "1.2450",
        unpriced: 0,
        durationMs: 40_000,
        measuredOutputs: 30_000,
        generationMs: 35_000,
      },
      providers: [],
      models: [],
      detailSince: 0,
      sources: { antigravity: 15 },
    };

    const usageState: UsageState = {
      settings: {
        recording: true,
        autoSync: true,
        refreshSeconds: 0,
        multiplier: "1",
        pricingModel: "response",
      },
      syncing: false,
      reports: {},
      error: null,
    };

    const sampleRow: UsageRecord = {
      id: "ag-req-1",
      client: "antigravity",
      source: "antigravity",
      startedAt: Date.UTC(2026, 9, 30, 8, 29, 0),
      finishedAt: Date.UTC(2026, 9, 30, 8, 29, 4),
      sessionId: null,
      completed: true,
      duplicateOf: null,
      deduplication: "独立记录",
      attempts: [
        {
          id: "ag-req-1-final",
          provider: null,
          requestedModel: "gemini-3.1-pro",
          responseModel: "gemini-3.1-pro",
          pricingModel: "gemini-3.1-pro",
          modelDisplayName: "Gemini 3.1 Pro",
          reasoningEffort: "high",
          responseId: null,
          status: null,
          outcome: "success",
          startedAt: Date.UTC(2026, 9, 30, 8, 29, 0),
          durationMs: 4_000,
          firstTokenMs: 320,
          stream: true,
          transport: "local_api",
          tokens: {
            input: 4_000,
            output: 1_200,
            reasoning: 600,
            cacheRead: 16_000,
            cacheWrite: 0,
          },
          serviceTier: null,
          price: {
            version: "seed",
            source: "sub2api",
            model: "gemini-3.1-pro",
            multiplier: "1",
            rates: {},
            cost: "0.0252",
          },
        },
      ],
    };

    mock.command.mockImplementation(
      async (name: string) => {
        if (name === "get_usage_dashboard") return dashboard;
        if (name === "get_usage_state") return usageState;
        if (name === "get_gateway") return { providers: [] };
        if (name === "get_antigravity_auth_state")
          return {
            accounts: [
              {
                id: "ag-oauth-1",
                name: "oauth@example.invalid",
                current: true,
              },
              { id: "ag-oauth-2", name: "backup@example.invalid" },
            ],
          };
        if (
          name === "get_antigravity_usage_stats" ||
          name === "get_claude_usage_stats"
        )
          return statsPayload;
        if (name === "refresh_antigravity_usage_quota")
          return quotaAnswers.shift() ?? { changed: false, error: null };
        if (name === "get_claude_usage_quota")
          return {
            account: "f***@example.invalid",
            plan: "Max",
            windows: [
              {
                id: "five_hour",
                label: "当前 5h",
                usedPercent: 22.5,
                resetsAt: null,
              },
            ],
            error: null,
          };
        if (name === "get_usage_logs") {
          return {
            rows: [sampleRow],
            total: 1,
            page: 1,
            detailSince: 0,
          };
        }
        throw new Error(`unexpected command: ${name}`);
      },
    );
  });

  it("locks each independent page to its client and clears incompatible filters", async () => {
    render(<Usage />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("tab", { name: "Antigravity" }));
    expect(screen.getByLabelText("用量客户端")).toBeDisabled();
    expect(screen.getByLabelText("用量客户端")).toHaveValue("antigravity");
    await user.selectOptions(screen.getByLabelText("用量供应商"), "ag-oauth-1");
    await user.selectOptions(screen.getByLabelText("请求类型"), "compaction");
    await user.click(screen.getByRole("tab", { name: "Claude Code" }));
    const claudeQuota = await screen.findByRole("region", {
      name: "Claude 官方额度",
    });
    expect(within(claudeQuota).getByText("最紧剩余 77.5%")).toBeInTheDocument();
    expect(within(claudeQuota).getByText("剩余 77.5%")).toBeInTheDocument();
    expect(within(claudeQuota).getByText("已用 22.5%")).toBeInTheDocument();
    expect(screen.getByLabelText("用量客户端")).toHaveValue("claude");
    expect(screen.getByLabelText("用量供应商")).toHaveValue("");
    expect(mock.command).toHaveBeenCalledWith("get_claude_usage_stats", {
      filter: expect.objectContaining({
        client: "claude",
        operation: "compaction",
        provider: undefined,
      }),
    });
    await user.click(screen.getByRole("tab", { name: "Antigravity" }));
    expect(mock.command).toHaveBeenCalledWith("get_antigravity_usage_stats", {
      filter: expect.objectContaining({
        client: "antigravity",
        operation: "compaction",
        provider: undefined,
      }),
    });
  });

  it("keeps local statistics usable on quota errors and stops hidden refreshes", async () => {
    const original = mock.command.getMockImplementation()!;
    mock.command.mockImplementation(
      (name: string, args?: Record<string, unknown>) =>
        name === "get_claude_usage_quota"
          ? Promise.reject(new Error("Claude 登录已失效"))
          : original(name, args),
    );
    render(<Usage />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("tab", { name: "Claude Code" }));
    expect(await screen.findByText("Claude 登录已失效")).toBeInTheDocument();
    const claudeQuota = screen.getByRole("region", { name: "Claude 官方额度" });
    expect(claudeQuota.querySelector(".usage-claude-meta")).toBeNull();
    expect(claudeQuota.querySelector(".usage-claude-windows")).toBeNull();
    expect(
      screen.getByRole("region", { name: "Token 构成与缓存节省" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "刷新用量" }));
    expect(mock.command).toHaveBeenCalledWith("get_claude_usage_quota", {
      refresh: true,
    });
    const reads = () =>
      mock.command.mock.calls.filter(
        ([name]) => name === "get_claude_usage_quota",
      ).length;
    await act(async () =>
      mock.listeners
        .get("app-visibility")
        ?.forEach((listener) => listener(false)),
    );
    const hiddenReads = reads();
    await act(async () =>
      mock.listeners
        .get("claude-login-state")
        ?.forEach((listener) => listener({ phase: "complete" })),
    );
    expect(reads()).toBe(hiddenReads);
    await act(async () =>
      mock.listeners
        .get("app-visibility")
        ?.forEach((listener) => listener(true)),
    );
    expect(reads()).toBe(hiddenReads + 1);
  });

  it("lazily fetches Antigravity stats on tab selection, displays dual-track windows & token mix, and filters by provider", async () => {
    const user = userEvent.setup();
    render(<Usage />);

    await waitFor(() => {
      expect(mock.command).toHaveBeenCalledWith(
        "get_usage_logs",
        expect.anything(),
      );
    });
    expect(
      mock.command.mock.calls.some(
        ([name]) => name === "get_antigravity_usage_stats",
      ),
    ).toBe(false);

    await user.click(screen.getByRole("tab", { name: "Antigravity" }));

    await waitFor(() => {
      expect(mock.command).toHaveBeenCalledWith(
        "get_antigravity_usage_stats",
        expect.anything(),
      );
    });

    const summary = await screen.findByLabelText("Token 构成与缓存节省");
    await waitFor(() => {
      expect(mock.command).toHaveBeenCalledWith("get_antigravity_auth_state");
    });
    expect(
      mock.command.mock.calls.filter(
        ([name]) => name === "get_antigravity_usage_stats",
      ),
    ).toHaveLength(1);
    expect(within(summary).getByText("54.0%")).toBeInTheDocument();
    expect(within(summary).getByText("78.3%")).toBeInTheDocument();
    expect(within(summary).getByText("$0.4860")).toBeInTheDocument();
    expect(within(summary).queryByText("会话")).not.toBeInTheDocument();

    const tracks = screen.getByLabelText("账号周期用量");
    expect(within(tracks).getAllByText("当前 5h")).toHaveLength(2);
    expect(within(tracks).getAllByText("当前 7d")).toHaveLength(2);
    expect(within(tracks).getByText("最低剩余 74.5%")).toBeInTheDocument();
    expect(within(tracks).getByText("最低剩余 42.0%")).toBeInTheDocument();
    expect(within(tracks).getByText("配额剩余 74.5%")).toBeInTheDocument();
    expect(within(tracks).getByText("配额剩余 42.0%")).toBeInTheDocument();
    expect(within(tracks).getByText("$0.0840/h")).toHaveAttribute(
      "title",
      "0.0840/h",
    );
    expect(within(tracks).getByText("$0.0033/h")).toBeInTheDocument();
    expect(within(tracks).queryByText(/Token\/h|K\/h/)).not.toBeInTheDocument();

    const creds = screen.getByLabelText("账号分布");
    expect(
      within(creds).getByText("oauth@example.invalid"),
    ).toBeInTheDocument();
    expect(
      within(creds).getByText("backup@example.invalid"),
    ).toBeInTheDocument();
    await user.click(
      within(creds).getByRole("button", {
        name: "按 oauth@example.invalid 筛选",
      }),
    );

    await waitFor(() => {
      expect(mock.command).toHaveBeenCalledWith("get_antigravity_usage_stats", {
        filter: expect.objectContaining({
          client: "antigravity",
          provider: "ag-oauth-1",
        }),
      });
    });

    const cancelBtn = within(creds).getByRole("button", {
      name: "取消按 oauth@example.invalid 筛选",
    });
    expect(cancelBtn).toHaveAttribute("aria-pressed", "true");
    await user.click(cancelBtn);

    await waitFor(() => {
      expect(screen.getByLabelText("用量供应商")).toHaveValue("");
    });
  });

  it("formats unpriced requests as 未定价 across summary, track windows, and provider rows", async () => {
    statsPayload = buildStats({
      costUsd: "0",
      unpricedRequests: 15,
      accounts: buildStats().accounts.map((a) => ({
        ...a,
        tracks: a.tracks.map((t) => ({
          ...t,
          burst5h: {
            ...t.burst5h,
            costUsd: "0",
            costPerHour: "0",
            unpricedRequests: t.burst5h.requests,
          },
          weekly7d: {
            ...t.weekly7d,
            costUsd: "0",
            costPerHour: "0",
            unpricedRequests: t.weekly7d.requests,
          },
        })),
      })),
      providers: buildStats().providers?.map((p) => ({
        ...p,
        costUsd: "0",
        unpricedRequests: p.requests,
      })),
    });
    const user = userEvent.setup();
    render(<Usage />);

    await user.click(screen.getByRole("tab", { name: "Antigravity" }));

    const summary = await screen.findByLabelText("Token 构成与缓存节省");
    expect(within(summary).getByText("未定价")).toBeInTheDocument();
    const tracks = screen.getByLabelText("账号周期用量");
    // Each window shows it twice: the cost badge and the hourly rate.
    expect(within(tracks).getAllByText("未定价")).toHaveLength(8);
    const creds = screen.getByLabelText("账号分布");
    expect(within(creds).getAllByText("未定价")).toHaveLength(2);
  });

  it("follows the app's current account without changing the global usage filter", async () => {
    statsPayload = {
      ...statsPayload,
      accounts: [
        ...statsPayload.accounts,
        {
          ...statsPayload.accounts[0],
          id: "ag-oauth-2",
          name: "Fixture Backup",
        },
      ],
    };
    const user = userEvent.setup();
    render(<Usage />);
    await user.click(screen.getByRole("tab", { name: "Antigravity" }));
    const select = await screen.findByRole("combobox", {
      name: "Antigravity 用量账号",
    });
    await waitFor(() => expect(select).toHaveValue("ag-oauth-1"));
    const statsReads = () =>
      mock.command.mock.calls.filter(
        ([name]) => name === "get_antigravity_usage_stats",
      ).length;
    const reads = statsReads();
    act(() =>
      mock.listeners.get("antigravity-auth-state")?.forEach((listener) =>
        listener({
          revision: "ag-rev-2",
          accounts: [
            { id: "ag-oauth-1", name: "oauth@example.invalid", current: false },
            { id: "ag-oauth-2", name: "backup@example.invalid", current: true },
          ],
        }),
      ),
    );
    await waitFor(() => expect(select).toHaveValue("ag-oauth-2"));
    await waitFor(() => expect(statsReads()).toBeGreaterThan(reads));
    const count = mock.command.mock.calls.length;
    await user.selectOptions(select, "ag-oauth-1");
    expect(mock.command.mock.calls).toHaveLength(count);
    expect(screen.getByRole("combobox", { name: "用量供应商" })).toHaveValue(
      "",
    );
  });

  const quotaCommand = "refresh_antigravity_usage_quota";
  const statsCommand = "get_antigravity_usage_stats";
  const periods = () => screen.getByLabelText("账号周期用量");
  /** The page on the Antigravity tab with its statistics read. */
  const open = async (user: ReturnType<typeof userEvent.setup>) => {
    const view = render(<Usage />);
    await user.click(screen.getByRole("tab", { name: "Antigravity" }));
    await screen.findByLabelText("账号周期用量");
    return view;
  };

  it("shows the history while the quota is asked for, and a failure beside the periods until a later answer", async () => {
    const answer = asking();
    const user = userEvent.setup();
    await open(user);

    // The history and the saved periods do not wait for the answer.
    expect(within(periods()).getByText("配额剩余 74.5%")).toBeInTheDocument();
    expect(
      within(screen.getByLabelText("Token 构成与缓存节省")).getByText("54.0%"),
    ).toBeInTheDocument();
    await waitFor(() => expect(calls(quotaCommand)).toHaveLength(1));
    expect(calls(quotaCommand)[0]).toEqual([quotaCommand]);
    const order = mock.command.mock.calls.map(([name]) => name);
    expect(order.indexOf(quotaCommand)).toBeGreaterThan(
      order.indexOf(statsCommand),
    );
    const refresh = screen.getByRole("button", { name: "刷新用量" });
    expect(refresh).toBeEnabled();
    expect(within(periods()).queryByRole("alert")).not.toBeInTheDocument();

    // Refreshing by hand meanwhile reads the history again and asks nothing more.
    await user.click(refresh);
    await waitFor(() => expect(calls(statsCommand)).toHaveLength(2));
    await waitFor(() => expect(refresh).toBeEnabled());
    expect(calls(quotaCommand)).toHaveLength(1);

    await act(async () =>
      answer({ changed: false, error: "agy 读取额度超时，请稍后再试" }),
    );
    expect(within(periods()).getByRole("alert")).toHaveTextContent(
      "agy 读取额度超时，请稍后再试",
    );
    // The saved periods are the ones on the page, so nothing is read again.
    expect(calls(statsCommand)).toHaveLength(2);

    // The next answer takes the failure away.
    await user.click(refresh);
    await waitFor(() => expect(calls(quotaCommand)).toHaveLength(2));
    await waitFor(() =>
      expect(within(periods()).queryByRole("alert")).not.toBeInTheDocument(),
    );
  });

  it("reads the statistics again once the answer says the saved periods changed", async () => {
    const answer = asking();
    const user = userEvent.setup();
    await open(user);
    expect(within(periods()).getByText("配额剩余 74.5%")).toBeInTheDocument();
    await waitFor(() => expect(calls(quotaCommand)).toHaveLength(1));
    expect(calls(statsCommand)).toHaveLength(1);

    const next = buildStats();
    const window = next.accounts[0].tracks[0].burst5h;
    window.remainingPercent = 61;
    window.period!.remainingPercent = 61;
    statsPayload = next;
    await act(async () => answer({ changed: true, error: null }));
    expect(
      await within(periods()).findByText("配额剩余 61.0%"),
    ).toBeInTheDocument();
    expect(calls(statsCommand)).toHaveLength(2);
    // What is read after a change asks for nothing again.
    expect(calls(quotaCommand)).toHaveLength(1);
  });

  it("drops an answer that arrives after the tab was left, and asks nothing for Claude Code", async () => {
    const answer = asking();
    const user = userEvent.setup();
    await open(user);
    await waitFor(() => expect(calls(quotaCommand)).toHaveLength(1));

    await user.click(screen.getByRole("tab", { name: "Claude Code" }));
    await screen.findByRole("region", { name: "Claude 官方额度" });
    await screen.findByLabelText("Token 构成与缓存节省");
    await user.click(screen.getByRole("button", { name: "刷新用量" }));
    await waitFor(() =>
      expect(calls("get_claude_usage_stats")).toHaveLength(2),
    );
    await act(async () =>
      answer({ changed: true, error: "迟到的额度读取失败" }),
    );
    expect(calls(statsCommand)).toHaveLength(1);
    expect(calls("get_claude_usage_stats")).toHaveLength(2);
    expect(calls(quotaCommand)).toHaveLength(1);

    // Back on the tab the quota is asked for again; the late failure is gone.
    await user.click(screen.getByRole("tab", { name: "Antigravity" }));
    await screen.findByLabelText("账号周期用量");
    await waitFor(() => expect(calls(quotaCommand)).toHaveLength(2));
    expect(screen.queryByText("迟到的额度读取失败")).not.toBeInTheDocument();
  });

  it("asks an instance once at a time, and its answer never reaches the page of another", async () => {
    const answer = asking();
    const user = userEvent.setup();
    const first = await open(user);
    await waitFor(() => expect(calls(quotaCommand)).toHaveLength(1));
    first.unmount();

    // Opened again, the page waits for the answer already asked for.
    const second = await open(user);
    await waitFor(() => expect(calls(statsCommand)).toHaveLength(2));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "刷新用量" })).toBeEnabled(),
    );
    expect(calls(quotaCommand)).toHaveLength(1);
    second.unmount();

    // Another instance is asked for itself.
    act(() => setInstanceState(instances("m1")));
    await open(user);
    await waitFor(() => expect(calls(quotaCommand)).toHaveLength(2));
    const reads = calls(statsCommand).length;
    await act(async () =>
      answer({ changed: true, error: "本机的额度读取失败" }),
    );
    expect(screen.queryByText("本机的额度读取失败")).not.toBeInTheDocument();
    expect(calls(statsCommand)).toHaveLength(reads);
  });

  it("leaves the quota to a server that still asks for it while it reads the statistics", async () => {
    act(() => setInstanceState(instances("m1", [quotaCommand])));
    statsPayload = buildStats({ quotaError: "agy 读取额度超时，请稍后再试" });
    const user = userEvent.setup();
    await open(user);
    expect(within(periods()).getByRole("alert")).toHaveTextContent(
      "agy 读取额度超时，请稍后再试",
    );
    const refresh = screen.getByRole("button", { name: "刷新用量" });
    await user.click(refresh);
    await waitFor(() => expect(calls(statsCommand)).toHaveLength(2));
    await waitFor(() => expect(refresh).toBeEnabled());
    expect(calls(quotaCommand)).toHaveLength(0);
  });

  it("asks for the quota again after the history of each timed refresh", async () => {
    vi.useFakeTimers();
    const original = mock.command.getMockImplementation()!;
    mock.command.mockImplementation(
      (name: string, args?: Record<string, unknown>) =>
        name === "get_usage_state"
          ? Promise.resolve({
              settings: { ...settings, refreshSeconds: 5 },
              syncing: false,
              reports: {},
              error: null,
            })
          : original(name, args),
    );
    const settle = () =>
      act(async () => {
        for (let turn = 0; turn < 20; turn++) await Promise.resolve();
      });
    render(<Usage />);
    await settle();
    fireEvent.click(screen.getByRole("tab", { name: "Antigravity" }));
    await settle();
    expect(calls(statsCommand)).toHaveLength(1);
    expect(calls(quotaCommand)).toHaveLength(1);

    await act(async () => vi.advanceTimersByTime(5_000));
    await settle();
    expect(calls(statsCommand)).toHaveLength(2);
    expect(calls(quotaCommand)).toHaveLength(2);
    const order = mock.command.mock.calls.map(([name]) => name);
    expect(order.lastIndexOf(quotaCommand)).toBeGreaterThan(
      order.lastIndexOf(statsCommand),
    );
  });
});

it("a quota reported without a period says why in place of the period", () => {
  const base = buildStats();
  const [gemini, thirdParty] = base.accounts[0].tracks;
  const account = {
    ...base.accounts[0],
    tracks: [
      gemini,
      {
        ...thirdParty,
        burst5h: {
          ...thirdParty.burst5h,
          period: null,
          without: "disabled" as const,
        },
        weekly7d: {
          ...thirdParty.weekly7d,
          period: null,
          without: "noReset" as const,
          remainingPercent: 100,
        },
      },
    ],
  };
  render(
    <UsageAntigravity
      stats={{ ...base, accounts: [account] }}
      busy={false}
      currentAccountId={account.id}
    />,
  );
  const card = within(screen.getByLabelText(account.name));
  expect(card.getByText("此账号未开放")).toBeInTheDocument();
  expect(card.getByText("未返回重置时间 · 剩余 100.0%")).toBeInTheDocument();
  expect(card.queryByText("未获取当前周期")).not.toBeInTheDocument();
});

it("selects one account, retains manual selection on refresh, follows the current account and handles deletion", async () => {
  const user = userEvent.setup();
  const base = buildStats();
  const unavailable = {
    ...base.accounts[0],
    id: "b",
    name: "Fixture B",
    tracks: base.accounts[0].tracks.map((t) => ({
      ...t,
      burst5h: { ...t.burst5h, period: null },
      weekly7d: { ...t.weekly7d, period: null },
    })),
  };
  const accounts = [
    ...base.accounts,
    unavailable,
    { ...base.accounts[0], id: "c", name: "Fixture C" },
  ];
  const props = {
    stats: { ...base, accounts },
    busy: false,
    currentAccountId: "ag-oauth-1",
  };
  const view = render(<UsageAntigravity {...props} />);
  const select = screen.getByRole("combobox", { name: "Antigravity 用量账号" });
  expect(select).toHaveValue("ag-oauth-1");
  // Options carry the account name only; no account type is shown.
  expect(
    [...select.querySelectorAll("option")].map((option) => option.textContent),
  ).toEqual(["选择账号", "Fixture OAuth（当前）", "Fixture B", "Fixture C"]);
  expect(screen.queryByLabelText("Fixture B")).not.toBeInTheDocument();
  const known = screen.getByLabelText("Fixture OAuth");
  expect(within(known).getAllByText("当前 5h")).toHaveLength(2);
  expect(within(known).getAllByText("当前 7d")).toHaveLength(2);
  expect(known.querySelectorAll(".usage-ag-period")).toHaveLength(4);
  await user.selectOptions(select, "b");
  expect(screen.queryByLabelText("Fixture OAuth")).not.toBeInTheDocument();
  view.rerender(<UsageAntigravity {...props} stats={{ ...props.stats }} />);
  expect(select).toHaveValue("b");
  expect(
    within(screen.getByLabelText("Fixture B")).getAllByText("未获取当前周期"),
  ).toHaveLength(4);
  // Every account uses the two current-period cards; no date-range summary.
  await user.selectOptions(select, "c");
  const third = screen.getByLabelText("Fixture C");
  expect(third.querySelectorAll(".usage-ag-track-card")).toHaveLength(2);
  expect(within(third).getAllByText("当前 5h")).toHaveLength(2);
  expect(within(third).getAllByText("当前 7d")).toHaveLength(2);
  expect(screen.getByLabelText("Token 构成与缓存节省")).toHaveTextContent(
    "$1.2450",
  );
  view.rerender(<UsageAntigravity {...props} currentAccountId="b" />);
  expect(select).toHaveValue("b");
  await user.selectOptions(select, "c");
  view.rerender(
    <UsageAntigravity
      {...props}
      currentAccountId="b"
      stats={{ ...base, accounts: accounts.filter((a) => a.id !== "c") }}
    />,
  );
  expect(select).toHaveValue("b");
  view.rerender(<UsageAntigravity {...props} currentAccountId={null} />);
  expect(select).toHaveValue("");
  expect(screen.queryByLabelText("Fixture B")).not.toBeInTheDocument();
  view.rerender(
    <UsageAntigravity
      {...props}
      currentAccountId={null}
      stats={{ ...base, accounts: [] }}
    />,
  );
  expect(select).toBeDisabled();
  expect(screen.getByRole("option", { name: "暂无账号" })).toBeInTheDocument();
});

it("shows missing Claude reasoning independently of known token and savings totals", () => {
  const stats = buildStats();
  stats.tokenMix.reasoningReported = false;
  stats.tokenMix.thinkingSharePercent = null;
  stats.tokenMix.cacheSavingsUsd = "0";
  stats.tokenMix.cacheWrite = 0;
  render(
    <UsageAntigravity
      client="claude"
      stats={stats}
      busy={false}
      currentAccountId={null}
    />,
  );
  const mix = screen.getByRole("region", { name: "Token 构成与缓存节省" });
  expect(mix.querySelectorAll(".usage-ag-kpis > div")).toHaveLength(5);
  expect(within(mix).queryByText("可见输出")).not.toBeInTheDocument();
  expect(within(mix).getByText("输出")).toBeInTheDocument();
  expect(within(mix).getAllByText("—")).toHaveLength(1);
  expect(within(mix).getByText("$0.0000")).toBeInTheDocument();
  expect(within(mix).getByText("0")).toBeInTheDocument();
  expect(
    screen.queryByLabelText("Antigravity 用量账号"),
  ).not.toBeInTheDocument();
  const models = screen.getByRole("region", { name: "模型分布" });
  expect(within(models).getByText("Gemini")).toHaveAttribute("title", "Gemini");
});

