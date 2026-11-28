import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UsageCurves, UsageFilter } from "./usage-types";

import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
vi.mock("./confirmation", () => ({ confirmAction: mock.confirm }));

import Usage from "./Usage";
import UsageCurvesPanel from "./UsageCurves";

// Local midnight, so that the hours on the page are the same in every zone.
const start = new Date(2026, 9, 8).getTime();
const half = 30 * 60_000;
const none = { limited: 0, rejected: 0, server: 0, broken: 0 };
const names: Record<string, string> = {
  "fixture-limited": "Fixture Limited",
  "fixture-steady": "Fixture Steady",
  "fixture-top": "Fixture Top",
};
const name = (id: string) => names[id] ?? `供应商 ${id}`;
const host = "shared.example.invalid";

/** What an instance from before hosts and accounts answers with. */
function fixture(overrides: Partial<UsageCurves> = {}): UsageCurves {
  return {
    start,
    end: start + 48 * half,
    step: half,
    failuresSince: 0,
    journalError: null,
    providers: [
      {
        id: "fixture-limited",
        requests: 14,
        ok: 11,
        failures: { ...none, limited: 20, broken: 1 },
        trend: [0.2, 0.9, null, null, 0.5, ...Array(15).fill(null)],
        firstToken: { samples: 10, p50: 1200, p90: 3000 },
        summedUntil: null,
        buckets: [
          {
            at: start,
            ok: 8,
            failures: { ...none, limited: 2 },
            firstToken: { samples: 8, p50: 1200, p90: 3000 },
          },
          {
            at: start + half,
            ok: 2,
            failures: { ...none, limited: 18 },
            firstToken: { samples: 2, p50: 900, p90: 950 },
          },
          // Nothing ended in the hour between: the line stops and starts again.
          {
            at: start + 4 * half,
            ok: 1,
            failures: { ...none, broken: 1 },
            firstToken: { samples: 0, p50: null, p90: null },
          },
        ],
      },
      {
        id: "fixture-steady",
        requests: 40,
        ok: 40,
        failures: none,
        trend: [null, null, 0, ...Array(17).fill(null)],
        firstToken: { samples: 40, p50: 600, p90: 1500 },
        summedUntil: null,
        buckets: [
          {
            at: start + 2 * half,
            ok: 40,
            failures: none,
            firstToken: { samples: 40, p50: 600, p90: 1500 },
          },
        ],
      },
    ],
    ...overrides,
  };
}

/**
 * The two providers at one host, with the row that stands for both, and a
 * third at a host of its own that is rate-limited more than the two together.
 */
function hosted(): UsageCurves {
  const curves = fixture();
  const [limited, steady] = curves.providers;
  limited.host = host;
  steady.host = host;
  curves.providers.unshift({
    ...structuredClone(limited),
    id: "fixture-top",
    host: "top.example.invalid",
    failures: { ...none, limited: 90 },
  });
  curves.groups = [
    {
      id: `host:${host}`,
      host,
      members: [limited.id, steady.id],
      requests: 54,
      ok: 51,
      failures: { ...none, limited: 20, broken: 1 },
      trend: [0.2, 0.9, 0, null, 0.5, ...Array(15).fill(null)],
      firstToken: { samples: 50, p50: 700, p90: 2000 },
      summedUntil: null,
      buckets: [...limited.buckets, ...steady.buckets].sort(
        (a, b) => a.at - b.at,
      ),
    },
  ];
  return curves;
}

type Shown = ComponentProps<typeof UsageCurvesPanel>;
const shown = (props: Partial<Shown>): Shown => ({
  curves: null,
  stale: false,
  unread: false,
  unrecorded: false,
  selected: "",
  picked: "",
  split: [],
  name,
  onSelect: () => {},
  onPart: () => {},
  ...props,
});
/** The names of the rows to choose among, in the order they are listed. */
const listed = () =>
  [...document.querySelectorAll(".curve-pick-name")].map(
    (row) => row.textContent,
  );

function panel(
  curves: UsageCurves | null,
  selected = "",
  onSelect: Shown["onSelect"] = vi.fn(),
) {
  const view = render(
    <UsageCurvesPanel {...shown({ curves, selected, onSelect })} />,
  );
  const show = (next: string) =>
    view.rerender(
      <UsageCurvesPanel {...shown({ curves, selected: next, onSelect })} />,
    );
  const figures = () =>
    within(view.container.querySelector(".curve-figures") as HTMLElement);
  return { ...view, show, figures, onSelect };
}

describe("provider curves", () => {
  it("opens on the provider rate-limited the most and says what its rate is of", () => {
    const { figures, container } = panel(fixture());
    expect(
      screen.getByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    const picks = screen
      .getAllByRole("button")
      .filter((button) => button.classList.contains("curve-pick"));
    expect(picks.map((pick) => pick.getAttribute("aria-pressed"))).toEqual([
      "true",
      "false",
    ]);
    expect(picks[0]).toHaveTextContent("Fixture Limited");
    expect(picks[0]).toHaveTextContent("429 20 次 · 共 32");
    expect(picks[1]).toHaveTextContent("无失败 · 共 40");
    // The rate comes with the attempts it is a share of.
    expect(figures().getByText("65.6%")).toBeVisible();
    expect(figures().getByText("21 / 32 次尝试失败")).toBeVisible();
    expect(figures().getByText("占失败的 95.2%")).toBeVisible();
    expect(figures().getByText("1.20s")).toBeVisible();
    expect(figures().getByText("10 个样本")).toBeVisible();
    expect(figures().getByText("3.00s")).toBeVisible();
    const mix = within(screen.getByRole("list", { name: "失败构成" }));
    expect(mix.getByText("429 限流").closest("li")).toHaveTextContent(
      "429 限流2095.2%",
    );
    expect(mix.getByText("中断").closest("li")).toHaveTextContent("中断14.8%");
    expect(screen.getByText("429 峰值 18 次")).toBeVisible();
    // The journal reaches the whole range: nothing says from when it counts.
    expect(figures().queryByText(/^自 /)).not.toBeInTheDocument();
    expect(screen.queryByText("早于运行日志")).not.toBeInTheDocument();
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
    // A figure that needs no count beside it stands alone.
    expect(figures().getByText("3.00s").closest("dd")).toHaveTextContent(
      /^3\.00s$/,
    );
    // Two buckets side by side are one stretch of line and the one an hour
    // later another: no line is drawn across the time nothing ended in.
    const total = container.querySelector('.curve-line[data-line="total"]');
    expect(total?.getAttribute("d")?.match(/M/g)).toHaveLength(2);
  });

  it("shows another provider when the page names it", async () => {
    const user = userEvent.setup();
    const { figures, show, onSelect } = panel(fixture());
    await user.click(screen.getByRole("button", { name: /Fixture Steady/ }));
    expect(onSelect).toHaveBeenCalledWith("fixture-steady", "fixture-steady");
    show("fixture-steady");
    expect(
      screen.getByRole("heading", { name: "Fixture Steady" }),
    ).toBeVisible();
    expect(figures().getByText("0%")).toBeVisible();
    expect(figures().getByText("0 / 40 次尝试失败")).toBeVisible();
    expect(figures().getByText("没有失败")).toBeVisible();
    expect(screen.queryByText(/429 峰值/)).not.toBeInTheDocument();
  });

  it("narrows the list by name", async () => {
    const user = userEvent.setup();
    panel(fixture());
    const find = screen.getByRole("searchbox", { name: "筛选供应商" });
    await user.type(find, "STEA");
    expect(
      screen.queryByRole("button", { name: /Fixture Limited/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Fixture Steady/ }),
    ).toBeVisible();
    // The one drawn stays drawn while the list is searched.
    expect(
      screen.getByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    await user.clear(find);
    await user.type(find, "nowhere");
    expect(screen.getByText("没有名称包含“nowhere”的供应商")).toBeVisible();
  });

  it("reads each bucket from the keyboard", async () => {
    const user = userEvent.setup();
    panel(fixture());
    screen.getByRole("group", { name: "逐格读数，按左右方向键移动" }).focus();
    await user.keyboard("{ArrowRight}");
    const tip = () => within(screen.getByRole("status"));
    expect(tip().getByText("00:00–00:30")).toBeVisible();
    expect(tip().getByText("20%（2 / 10）")).toBeVisible();
    expect(tip().getByText("429 限流").closest("div")).toHaveTextContent(
      "429 限流2",
    );
    expect(tip().getByText("成功").closest("div")).toHaveTextContent("成功8");
    expect(tip().getByText("1.20s")).toBeVisible();
    await user.keyboard("{ArrowRight}");
    expect(tip().getByText("90%（18 / 20）")).toBeVisible();
    // How many first tokens the times are of is told beside them.
    expect(tip().getByText("首字样本").closest("div")).toHaveTextContent(
      "首字样本2",
    );
    await user.keyboard("{End}");
    expect(tip().getByText("02:00–02:30")).toBeVisible();
    expect(tip().getByText("50%（1 / 2）")).toBeVisible();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("has no error rate before the journal reaches, rather than a rate of zero", async () => {
    const user = userEvent.setup();
    const curves = fixture({ failuresSince: start + 4 * half });
    const limited = curves.providers[0];
    limited.ok = 1;
    limited.failures = { ...none, broken: 1 };
    limited.buckets[0].failures = null;
    limited.buckets[1].failures = null;
    const { figures } = panel(curves);
    expect(screen.getByText("早于运行日志")).toBeVisible();
    expect(figures().getByText("1 / 2 次尝试失败")).toBeVisible();
    // The rate and the 429s are of the time since the journal began, and say
    // so; the first tokens are of the whole range.
    const since = figures().getAllByText("自 10-08 02:00 起");
    expect(
      since.map(
        (line) => line.closest("div")?.querySelector("dt")?.textContent,
      ),
    ).toEqual(["错误率", "429 限流"]);
    await user.click(screen.getByRole("button", { name: "表格" }));
    const rows = screen.getAllByRole("row").slice(1);
    expect(rows).toHaveLength(3);
    // Eight requests succeeded there; how many failed is not known.
    expect(rows[0]).toHaveTextContent(
      "00:00–00:308不可知不可知不可知不可知不可知1.20s3.00s8",
    );
    expect(rows[2]).toHaveTextContent("02:00–02:301000150%——0");
  });

  it("says nothing of when the journal begins where it begins with the range", () => {
    // Seven days back, the range and the journal both begin a moment into
    // the first bucket.
    const { figures } = panel(fixture({ failuresSince: start + 60_000 }));
    expect(figures().getByText("21 / 32 次尝试失败")).toBeVisible();
    expect(figures().queryByText(/^自 /)).not.toBeInTheDocument();
  });

  it("does not say none failed where the journal reaches none of the range", () => {
    // The journal begins as the range ends.
    const curves = fixture({ failuresSince: start + 48 * half });
    for (const provider of curves.providers) {
      provider.ok = 0;
      provider.failures = none;
      provider.trend = provider.trend.map(() => null);
      for (const bucket of provider.buckets) bucket.failures = null;
    }
    const { figures } = panel(curves, "fixture-limited");
    const unknown = figures().getAllByText("不可知");
    expect(
      unknown.map(
        (value) => value.closest("div")?.querySelector("dt")?.textContent,
      ),
    ).toEqual(["错误率", "429 限流"]);
    expect(figures().getAllByText("早于运行日志")).toHaveLength(2);
    expect(figures().queryByText("没有失败")).not.toBeInTheDocument();
    expect(figures().queryByText(/^自 /)).not.toBeInTheDocument();
    // No four kinds of failure with none of each.
    expect(
      screen.queryByRole("list", { name: "失败构成" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("这段时间早于运行日志，失败构成不可知"),
    ).toBeVisible();
    // What the records hold is told all the same.
    expect(figures().getByText("1.20s")).toBeVisible();
    expect(figures().getByText("10 个样本")).toBeVisible();
  });

  it("marks the time only the chosen provider's sums by day are left of", () => {
    const curves = fixture();
    curves.providers[0].summedUntil = start + 12 * half;
    const { container, show } = panel(curves);
    const hatch = screen.getByText("只留有日汇总").parentElement;
    expect(hatch?.parentElement).toHaveAttribute("data-plot", "time");
    expect(hatch).toHaveStyle({ width: "25%" });
    // The other has every request of the range one by one.
    show("fixture-steady");
    expect(screen.queryByText("只留有日汇总")).not.toBeInTheDocument();
    expect(container.querySelector(".curve-blind")).not.toBeInTheDocument();
  });

  it("reads nothing out where the pointer is far from every bucket", () => {
    const { container } = panel(fixture());
    const plot = container.querySelector('[data-plot="rate"]') as HTMLElement;
    vi.spyOn(plot, "getBoundingClientRect").mockReturnValue({
      left: 0,
      width: 960,
    } as DOMRect);
    // Over the first half hour, and then between the second and the fifth.
    fireEvent.pointerMove(plot, { clientX: 10 });
    expect(
      within(screen.getByRole("status")).getByText("00:00–00:30"),
    ).toBeVisible();
    fireEvent.pointerMove(plot, { clientX: 58 });
    expect(
      within(screen.getByRole("status")).getByText("00:30–01:00"),
    ).toBeVisible();
    // Nothing ended in the afternoon: the 429s of the night are not its.
    fireEvent.pointerMove(plot, { clientX: 600 });
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("says what leaves the figures short of what happened", () => {
    const { rerender } = panel(fixture());
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
    rerender(
      <UsageCurvesPanel
        {...shown({
          curves: fixture({ journalError: "运行日志部分记录损坏，已跳过" }),
          unrecorded: true,
        })}
      />,
    );
    expect(screen.getAllByRole("note").map((note) => note.textContent)).toEqual(
      ["运行日志有异常（运行日志部分记录损坏，已跳过）", "网关记录已关闭"],
    );
  });

  it("says so where there is nothing to draw", () => {
    const { rerender } = panel(null);
    expect(screen.getByText("正在读取曲线…")).toBeVisible();
    // Reading them failed: nothing is on its way.
    rerender(<UsageCurvesPanel {...shown({ unread: true })} />);
    expect(screen.getByRole("alert")).toHaveTextContent(
      "曲线没有读到，点右上角“刷新用量”重试",
    );
    expect(screen.queryByText("正在读取曲线…")).not.toBeInTheDocument();
    rerender(
      <UsageCurvesPanel {...shown({ curves: fixture({ providers: [] }) })} />,
    );
    expect(screen.getByText(/这段时间没有可统计的请求/)).toBeVisible();
    // The page is narrowed to one that did nothing in the range: the others
    // are still there to choose.
    rerender(
      <UsageCurvesPanel
        {...shown({ curves: fixture(), selected: "fixture-idle" })}
      />,
    );
    expect(
      screen.getByText(/“供应商 fixture-idle”在这段时间没有可统计的请求/),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /Fixture Limited/ }),
    ).toHaveAttribute("aria-pressed", "false");
  });

  it("shows the providers of one host as one row until they are taken apart", async () => {
    const user = userEvent.setup();
    const { figures, rerender } = panel(hosted(), "fixture-steady");
    expect(listed()).toEqual(["Fixture Top", host]);
    // Narrowed to one of the two, the page shows the row that holds it: named
    // by the host, and telling which providers it stands for.
    const heading = screen.getByRole("heading", { name: host });
    expect(heading).toHaveAttribute("title", "Fixture Limited、Fixture Steady");
    expect(figures().getByText("21 / 72 次尝试失败")).toBeVisible();
    expect(screen.getByRole("button", { name: "拆分" })).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "合并同域" }),
    ).not.toBeInTheDocument();
    // The host is found by the name of either.
    const find = screen.getByRole("searchbox", { name: "筛选供应商" });
    await user.type(find, "stea");
    expect(listed()).toEqual([host]);
    await user.clear(find);

    rerender(
      <UsageCurvesPanel
        {...shown({
          curves: hosted(),
          selected: "fixture-steady",
          split: [host],
        })}
      />,
    );
    expect(listed()).toEqual([
      "Fixture Top",
      "Fixture Limited",
      "Fixture Steady",
    ]);
    expect(
      screen.getByRole("heading", { name: "Fixture Steady" }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "合并同域" })).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "拆分" }),
    ).not.toBeInTheDocument();
    // One with a host to itself has nothing to be taken apart from.
    rerender(
      <UsageCurvesPanel
        {...shown({ curves: hosted(), selected: "fixture-top" })}
      />,
    );
    expect(screen.getByRole("heading", { name: "Fixture Top" })).toBeVisible();
    expect(
      screen.queryByRole("button", { name: /拆分|合并同域/ }),
    ).not.toBeInTheDocument();
  });

  it("does not bound by the journal what the records tell of", () => {
    // The journal begins as the range ends and is damaged, and the gateway
    // records nothing: none of it is of what never passed through a gateway.
    const curves = fixture({
      failuresSince: start + 48 * half,
      journalError: "运行日志部分记录损坏，已跳过",
    });
    for (const provider of curves.providers) {
      provider.ok = 0;
      provider.failures = none;
      for (const bucket of provider.buckets) bucket.failures = null;
    }
    curves.direct = [
      {
        id: "session:claude",
        recorded: true,
        requests: 9,
        ok: 8,
        failures: { ...none, limited: 1 },
        trend: [0.5, 0, ...Array(18).fill(null)],
        firstToken: { samples: 0, p50: null, p90: null },
        summedUntil: null,
        buckets: [
          {
            at: start,
            ok: 1,
            failures: { ...none, limited: 1 },
            firstToken: { samples: 0, p50: null, p90: null },
          },
          {
            at: start + half,
            ok: 7,
            failures: none,
            firstToken: { samples: 0, p50: null, p90: null },
          },
        ],
      },
      {
        ...structuredClone(curves.providers[1]),
        id: "fixture-account",
        recorded: true,
        requests: 3,
        ok: 0,
        failures: none,
        trend: Array(20).fill(null),
        buckets: [],
      },
    ];
    const view = panel(curves, "session");
    // The sessions of a client that belong to no account are named by it.
    expect(
      screen.getByRole("heading", { name: "Claude Code 本机会话" }),
    ).toBeVisible();
    // With no failure known of the others, the busier comes first.
    expect(listed()).toEqual([
      "Claude Code 本机会话",
      "Fixture Steady",
      "Fixture Limited",
      "供应商 fixture-account",
    ]);
    expect(view.figures().getByText("11.1%")).toBeVisible();
    expect(view.figures().getByText("1 / 9 次尝试失败")).toBeVisible();
    expect(view.figures().queryByText("不可知")).not.toBeInTheDocument();
    expect(screen.queryByText("早于运行日志")).not.toBeInTheDocument();
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
    // Of an account nothing is known to have ended either way: that is said
    // of its results, the journal having nothing to do with it.
    expect(
      screen.getByRole("button", { name: /供应商 fixture-account/ }),
    ).toHaveTextContent("结果未知 · 3 次请求");
    expect(
      screen.getByRole("button", { name: /Fixture Steady/ }),
    ).toHaveTextContent("失败不可知 · 40 次请求");
    // A provider of the gateway beside it is bounded as before.
    view.rerender(
      <UsageCurvesPanel
        {...shown({ curves, unrecorded: true, picked: "fixture-limited" })}
      />,
    );
    expect(view.figures().getAllByText("不可知")).toHaveLength(2);
    expect(screen.getAllByRole("note")).toHaveLength(2);
  });
});

describe("the curves tab of the usage page", () => {
  // What the instance answers a reading of the curves with, and its settings.
  let answer: () => UsageCurves | Promise<UsageCurves>;
  let settings: {
    recording: boolean;
    refreshSeconds: number;
    curveSplit?: string[];
  };
  const state = () => ({
    settings: {
      autoSync: true,
      multiplier: "1",
      pricingModel: "response",
      ...settings,
    },
    syncing: false,
    reports: {},
    error: null,
  });
  const asked = () =>
    mock.command.mock.calls.filter(
      ([command]) => command === "get_usage_curves",
    );
  const flush = () =>
    act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
  afterEach(() => vi.useRealTimers());
  beforeEach(() => {
    answer = () => fixture();
    settings = { recording: true, refreshSeconds: 0 };
    mock.command.mockReset();
    mock.confirm.mockReset();
    mock.listeners.clear();
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "visible",
    });
    mock.command.mockImplementation(
      async (command: string, args: Record<string, unknown> = {}) => {
        switch (command) {
          case "get_gateway":
            return {
              clientId: args.clientId,
              providers:
                args.clientId === "codex"
                  ? Object.entries(names).map(([id, name]) => ({ id, name }))
                  : [],
            };
          case "get_usage_dashboard":
            return {
              totals: {
                requests: 0,
                attempts: 0,
                success: 0,
                statusKnown: 0,
                sessions: 0,
                tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
                cost: "0",
                unpriced: 0,
                durationMs: 0,
                measuredOutputs: 0,
                generationMs: 0,
                firstTokenSumMs: 0,
                firstTokenSamples: 0,
                cacheReadEligible: 0,
                cacheInputEligible: 0,
              },
              providers: [],
              models: [],
              detailSince: 0,
              sources: {},
            };
          case "get_usage_logs":
            return { rows: [], total: 0, page: 1, detailSince: 0 };
          case "get_usage_state":
            return state();
          case "set_usage_settings":
            settings = args.settings as typeof settings;
            return state();
          case "get_usage_curves":
            return answer();
          default:
            throw new Error(`Unexpected fixture command: ${command}`);
        }
      },
    );
  });

  it("reads every provider's curves once and chooses among them without asking again", async () => {
    const user = userEvent.setup();
    render(<Usage />);
    await user.click(await screen.findByRole("tab", { name: "曲线" }));
    expect(
      await screen.findByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    expect(asked()).toHaveLength(1);
    // The time and the client and nothing else: no provider, type or key.
    const filter = (asked()[0][1] as { filter: UsageFilter }).filter;
    expect(Object.keys(filter).sort()).toEqual(["client", "end", "start"]);
    expect(filter.client).toBeUndefined();
    // The totals of the filtered requests are not the curves' and step aside.
    expect(screen.queryByText("估算费用")).not.toBeInTheDocument();
    expect(screen.getByLabelText("请求类型")).toBeDisabled();
    await user.click(screen.getByRole("button", { name: /Fixture Steady/ }));
    expect(
      await screen.findByRole("heading", { name: "Fixture Steady" }),
    ).toBeVisible();
    expect(screen.getByLabelText("用量供应商")).toHaveValue("fixture-steady");
    expect(asked()).toHaveLength(1);
    // Narrowed to it, the request log is of the provider that was looked at.
    await user.click(screen.getByRole("tab", { name: "请求日志" }));
    await waitFor(() =>
      expect(mock.command).toHaveBeenLastCalledWith(
        expect.stringMatching(/^get_usage_(logs|dashboard)$/),
        {
          filter: expect.objectContaining({ provider: "fixture-steady" }),
        },
      ),
    );
    expect(screen.getByText("估算费用")).toBeVisible();
  });

  it("shows nothing of another client where reading this one's curves failed", async () => {
    const user = userEvent.setup();
    render(<Usage />);
    await user.click(await screen.findByRole("tab", { name: "曲线" }));
    expect(
      await screen.findByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    answer = () => {
      throw new Error("曲线读取超时");
    };
    await user.selectOptions(screen.getByLabelText("用量客户端"), "codex");
    expect(
      await screen.findByText("曲线没有读到，点右上角“刷新用量”重试"),
    ).toHaveAttribute("role", "alert");
    // Those on the page were every client's: they are not Codex's to choose.
    expect(
      screen.queryByRole("heading", { name: "Fixture Limited" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Fixture Steady/ }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("正在读取曲线…")).not.toBeInTheDocument();
    expect(screen.getByText("曲线读取超时")).toBeVisible();

    answer = () => fixture();
    await user.click(screen.getByRole("button", { name: "刷新用量" }));
    expect(
      await screen.findByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    expect(screen.queryByText(/曲线没有读到/)).not.toBeInTheDocument();
    expect(screen.queryByText("曲线读取超时")).not.toBeInTheDocument();

    // Read again and failing, what was read of this client stays as it is.
    answer = () => {
      throw new Error("曲线读取超时");
    };
    await user.click(screen.getByRole("button", { name: "刷新用量" }));
    expect(await screen.findByText("曲线读取超时")).toBeVisible();
    expect(
      screen.getByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    expect(screen.queryByText(/曲线没有读到/)).not.toBeInTheDocument();
  });

  it("leaves a reading under way to answer when the next refresh comes round", async () => {
    vi.useFakeTimers();
    settings.refreshSeconds = 5;
    const answers: ((curves: UsageCurves) => void)[] = [];
    answer = () => new Promise((resolve) => answers.push(resolve));
    render(<Usage />);
    await flush();
    fireEvent.click(screen.getByRole("tab", { name: "曲线" }));
    await flush();
    expect(asked()).toHaveLength(1);
    // The range ends now, later at each refresh: each would be a new request.
    await act(async () => vi.advanceTimersByTime(10_000));
    expect(asked()).toHaveLength(1);
    await act(async () => answers[0](fixture()));
    expect(
      screen.getByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    // Answered, the next refresh reads them again.
    await act(async () => vi.advanceTimersByTime(5_000));
    expect(asked()).toHaveLength(2);
    // Another client's are another reading, whatever is under way.
    fireEvent.change(screen.getByLabelText("用量客户端"), {
      target: { value: "codex" },
    });
    await flush();
    expect(asked()).toHaveLength(3);
    expect((asked()[2][1] as { filter: UsageFilter }).filter.client).toBe(
      "codex",
    );
    // The answer for every client comes late and is not Codex's.
    await act(async () => answers[1](fixture({ providers: [] })));
    expect(
      screen.queryByText(/这段时间没有可统计的请求/),
    ).not.toBeInTheDocument();
    await act(async () =>
      answers[2](fixture({ providers: fixture().providers.slice(1) })),
    );
    expect(
      screen.getByRole("heading", { name: "Fixture Steady" }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: /Fixture Limited/ }),
    ).not.toBeInTheDocument();
  });

  it("names the provider the curves were narrowed to where it is not among the choices", async () => {
    const user = userEvent.setup();
    // The records know a provider no gateway has any more.
    answer = () => {
      const curves = fixture();
      curves.providers[1].id = "fixture-gone";
      return curves;
    };
    render(<Usage />);
    await user.click(await screen.findByRole("tab", { name: "曲线" }));
    await user.click(
      await screen.findByRole("button", { name: /供应商 fixture-/ }),
    );
    const choices = screen.getByLabelText("用量供应商");
    expect(choices).toHaveValue("fixture-gone");
    expect(
      within(choices).getByRole("option", { name: "供应商 fixture-" }),
    ).toHaveProperty("selected", true);
    // Under Antigravity the choices are its accounts, and the curves are of
    // the providers of the agy gateway.
    await user.selectOptions(
      screen.getByLabelText("用量客户端"),
      "antigravity",
    );
    expect(choices).toHaveValue("");
    await user.click(
      await screen.findByRole("button", { name: /Fixture Limited/ }),
    );
    expect(choices).toHaveValue("fixture-limited");
    expect(
      within(choices).getByRole("option", { name: "Fixture Limited" }),
    ).toHaveProperty("selected", true);
    // The session without a provider is listed once.
    expect(
      within(choices).getAllByRole("option", { name: /本机会话/ }),
    ).toHaveLength(1);
  });

  it("takes a host apart and puts it back, going to the row that stands for what was shown", async () => {
    const user = userEvent.setup();
    answer = hosted;
    const saved = () =>
      mock.command.mock.calls
        .filter(([command]) => command === "set_usage_settings")
        .map(([, args]) => (args as { settings: typeof settings }).settings);
    render(<Usage />);
    await user.click(await screen.findByRole("tab", { name: "曲线" }));
    // The host is second: the one rate-limited the most is shown first.
    expect(
      await screen.findByRole("heading", { name: "Fixture Top" }),
    ).toBeVisible();
    const choices = screen.getByLabelText("用量供应商");
    await user.click(screen.getByRole("button", { name: /Fixture Top/ }));
    expect(choices).toHaveValue("fixture-top");
    // A host is no provider: the page is narrowed to none while it is shown.
    await user.click(screen.getByRole("button", { name: new RegExp(host) }));
    expect(screen.getByRole("heading", { name: host })).toBeVisible();
    expect(choices).toHaveValue("");

    await user.click(screen.getByRole("button", { name: "拆分" }));
    expect(
      await screen.findByRole("heading", { name: "Fixture Limited" }),
    ).toBeVisible();
    expect(saved()).toEqual([
      expect.objectContaining({ recording: true, curveSplit: [host] }),
    ]);
    expect(listed()).toEqual([
      "Fixture Top",
      "Fixture Limited",
      "Fixture Steady",
    ]);
    expect(choices).toHaveValue("fixture-limited");

    await user.click(screen.getByRole("button", { name: /Fixture Steady/ }));
    await user.click(screen.getByRole("button", { name: "合并同域" }));
    expect(await screen.findByRole("heading", { name: host })).toBeVisible();
    expect(saved()[1]).toEqual(expect.objectContaining({ curveSplit: [] }));
    expect(listed()).toEqual(["Fixture Top", host]);
    expect(choices).toHaveValue("");
    // Chosen at the top of the page, a provider of the host shows the host.
    await user.click(screen.getByRole("button", { name: /Fixture Top/ }));
    await user.selectOptions(choices, "fixture-steady");
    expect(screen.getByRole("heading", { name: host })).toBeVisible();
    expect(asked()).toHaveLength(1);
  });

  it("narrows the page to the sessions without an account from their row", async () => {
    const user = userEvent.setup();
    answer = () => {
      const curves = fixture();
      curves.direct = [
        {
          ...structuredClone(curves.providers[1]),
          id: "session:antigravity",
          recorded: true,
        },
      ];
      return curves;
    };
    render(<Usage />);
    await user.click(await screen.findByRole("tab", { name: "曲线" }));
    await user.click(
      await screen.findByRole("button", {
        name: /Antigravity 本机会话（未归属）/,
      }),
    );
    expect(
      screen.getByRole("heading", { name: "Antigravity 本机会话（未归属）" }),
    ).toBeVisible();
    const choices = screen.getByLabelText("用量供应商");
    expect(choices).toHaveValue("session");
    expect(
      within(choices).getAllByRole("option", { name: /本机会话/ }),
    ).toHaveLength(1);
  });

  it("says that the gateway is not recording what it serves", async () => {
    const user = userEvent.setup();
    settings.recording = false;
    render(<Usage />);
    await user.click(await screen.findByRole("tab", { name: "曲线" }));
    expect(await screen.findByRole("note")).toHaveTextContent("网关记录已关闭");
  });
});
