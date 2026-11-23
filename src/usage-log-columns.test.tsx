import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import {
  cacheHitRate,
  columnVisible,
  interruption,
  logMoney,
  logNumber,
  logTiming,
  generationSpeed,
} from "./usage-log-columns";
import { modelRelation } from "./usage-format";
import type { Attempt, UsageRecord, UsageSettings } from "./usage-types";
import UsageLogTable, { UsageLogControls } from "./UsageLogTable";
import Detail from "./UsageDetail";
const defaultSettings: UsageSettings = {
  recording: true,
  autoSync: false,
  refreshSeconds: 0,
  multiplier: "1",
  pricingModel: "response",
};
const tokens = {
  input: 100,
  output: 60,
  cacheRead: 50,
  cacheWrite: 0,
  inclusiveInput: 150,
  reasoning: 20,
} as Attempt["tokens"];
const attempt = {
  tokens,
  durationMs: 2500,
  firstTokenMs: 500,
  stream: true,
  usageStatus: "reported",
  provider: "fixture",
  requestedModel: "fixture-request",
  responseModel: "fixture-response",
  status: 200,
  parseReasons: [],
} as unknown as Attempt;
const logSpeed = (a: Attempt, source = "proxy") => logTiming(a, source).speed;
describe("request log columns", () => {
  it("keeps total TPS independent of near-zero generation timing", () => {
    for (const generationMs of [0, 50, 99, null, undefined]) {
      expect(generationSpeed({ ...attempt, generationMs })).toBeNull();
    }
    expect(logSpeed({ ...attempt, firstTokenMs: 2499, generationMs: 1 })).toBe(
      24,
    );
  });
  it("takes generation speed from the output left after thinking", () => {
    const speed = (
      generationMs: number,
      firstTokenMs: number | null = 500,
      extra: Partial<Attempt["tokens"]> = {},
    ) =>
      generationSpeed({
        ...attempt,
        generationMs,
        firstTokenMs,
        tokens: { ...tokens, ...extra },
      });
    // 60 output tokens, 20 of them thinking.
    expect(speed(2000)).toBe(20);
    expect(speed(2000, 500, { reasoning: null })).toBe(30);
    // A window under a second counts only when the wait before it is at
    // most four times as long.
    expect(speed(500, 2000)).toBe(80);
    expect(speed(500, 2001)).toBeNull();
    expect(speed(500, null)).toBeNull();
    expect(speed(100, 400)).toBe(400);
    expect(speed(1000, 60_000)).toBe(40);
    // Nothing but thinking, or a speed no model reaches.
    expect(speed(2000, 500, { reasoning: 60 })).toBeNull();
    expect(speed(2000, 500, { reasoning: 80 })).toBeNull();
    expect(speed(100, 0, { output: 1020 })).toBe(10_000);
    expect(speed(100, 0, { output: 1021 })).toBeNull();
  });
  it("rates an Antigravity row only above 200 tokens and one second", () => {
    const row = (output: number, durationMs: number) => ({
      ...attempt,
      durationMs,
      tokens: { ...tokens, output },
    });
    expect(logTiming(row(200, 1000), "antigravity")).toEqual({
      duration: 1000,
      estimated: false,
      speed: 200,
      speedNote: null,
    });
    for (const small of [row(199, 1000), row(200, 999)]) {
      expect(logTiming(small, "antigravity")).toMatchObject({
        speed: null,
        speedNote: "输出不足 200 Token 或耗时不足 1 秒，不计算速度",
      });
      // Other sources are rated as before.
      for (const source of ["proxy", "session", "claude"])
        expect(logTiming(small, source)).toMatchObject({
          speed: (small.tokens.output! * 1000) / small.durationMs,
          speedNote: null,
        });
    }
    // Without a time there is nothing to explain.
    expect(
      logTiming(
        { ...row(60, 0), parseReasons: ["attempt_time_unreported"] },
        "antigravity",
      ),
    ).toEqual({
      duration: null,
      estimated: false,
      speed: null,
      speedNote: null,
    });
  });
  it("falls back to an estimated duration only where none was reported", () => {
    const unreported = {
      ...attempt,
      durationMs: 0,
      parseReasons: ["attempt_time_unreported"],
      tokens: { ...tokens, output: 1097 },
    };
    const estimated = logTiming(
      { ...unreported, estimatedDurationMs: 24_000 },
      "antigravity",
    );
    expect(estimated).toMatchObject({ duration: 24_000, estimated: true });
    expect(estimated.speed).toBeCloseTo(45.7, 1);
    for (const estimatedDurationMs of [null, undefined, 0])
      expect(
        logTiming({ ...unreported, estimatedDurationMs }, "antigravity"),
      ).toMatchObject({ duration: null, estimated: false, speed: null });
    expect(
      logTiming({ ...attempt, estimatedDurationMs: 24_000 }, "antigravity"),
    ).toMatchObject({ duration: 2500, estimated: false });
  });
  it("marks estimated time and speed in the list and the details", () => {
    const record = (id: string, source: string, a: Partial<Attempt>) =>
      ({
        id,
        client: source === "proxy" ? "claude" : "antigravity",
        source,
        startedAt: 1,
        attempts: [{ ...attempt, id, transport: "cli_file", ...a }],
      }) as UsageRecord;
    const estimated = record("fixture-estimated", "antigravity", {
      durationMs: 0,
      firstTokenMs: null,
      stream: false,
      parseReasons: ["attempt_time_unreported"],
      estimatedDurationMs: 24_000,
      tokens: { ...tokens, output: 1097 },
    });
    const small = record("fixture-small", "antigravity", { durationMs: 1500 });
    const gateway = record("fixture-gateway", "proxy", { durationMs: 1500 });
    const list = render(
      <UsageLogTable
        rows={[estimated, small, gateway]}
        busy={false}
        settings={defaultSettings}
        name={() => "Fixture"}
        open={() => {}}
      />,
    );
    const cell = (row: number, label: string) =>
      within(screen.getAllByRole("row")[row + 1]).getByLabelText(label)
        .lastElementChild!;
    expect(cell(0, "总耗时")).toHaveTextContent(/^≈24\.00s$/);
    expect(cell(0, "总耗时")).toHaveAttribute(
      "title",
      "≈24000 ms · 按下一步开始时间估算",
    );
    expect(cell(0, "输出 TPS")).toHaveTextContent(/^≈45\.7$/);
    expect(cell(0, "输出 TPS").getAttribute("title")).toMatch(
      /^≈45\.7\d* tok\/s · 按下一步开始时间估算$/,
    );
    expect(cell(1, "总耗时")).toHaveTextContent(/^1\.50s$/);
    expect(cell(1, "输出 TPS")).toHaveTextContent(/^—$/);
    expect(cell(1, "输出 TPS")).toHaveAttribute(
      "title",
      "输出不足 200 Token 或耗时不足 1 秒，不计算速度",
    );
    expect(cell(2, "输出 TPS")).toHaveTextContent(/^40\.0$/);
    expect(cell(2, "输出 TPS")).toHaveAttribute("title", "40 tok/s");
    list.unmount();

    const field = (label: string) =>
      screen.getByText(label, { selector: "dt" }).nextElementSibling!;
    const detail = render(
      <Detail record={estimated} name={() => "Fixture"} onClose={() => {}} />,
    );
    expect(field("总耗时")).toHaveTextContent(/^≈24\.000s$/);
    expect(field("总耗时").firstElementChild).toHaveAttribute(
      "title",
      "按下一步开始时间估算",
    );
    expect(field("输出 TPS")).toHaveTextContent(/^≈45\.7 tok\/s$/);
    expect(field("输出 TPS").firstElementChild).toHaveAttribute(
      "title",
      "按下一步开始时间估算",
    );
    expect(screen.getByText("生成 TPS", { selector: "dt" })).toHaveAttribute(
      "title",
      "（输出 − 思考）÷ 生成耗时",
    );
    detail.unmount();
    render(<Detail record={small} name={() => "Fixture"} onClose={() => {}} />);
    expect(field("总耗时")).toHaveTextContent(/^1\.500s$/);
    expect(field("输出 TPS").firstElementChild).toHaveAttribute(
      "title",
      "输出不足 200 Token 或耗时不足 1 秒，不计算速度",
    );
  });
  it("distinguishes unknown, zero, compact boundaries and exact costs", () => {
    expect([null, 0, 9999, 10000, 1050000].map((v) => logNumber(v))).toEqual([
      "—",
      "0",
      "9,999",
      "10.0K",
      "1.1M",
    ]);
    expect(logNumber(1050000, true)).toBe("1,050,000");
    expect(logMoney("0.000000123456")).toBe("$1.235e-7");
    expect(logMoney("0.000000123456", true)).toBe("$0.000000123456");
    expect(logMoney("0")).toBe("$0");
  });
  it("uses explicit choices before the grouped defaults and calculates only reported measures", () => {
    expect(columnVisible("time", undefined)).toBe(true);
    expect(columnVisible("speed", undefined)).toBe(true);
    expect(
      columnVisible("speed", {
        ...defaultSettings,
        logColumns: { speed: false },
      }),
    ).toBe(false);
    expect(
      columnVisible("speed", {
        ...defaultSettings,
        logColumns: { speed: true },
      }),
    ).toBe(true);
    expect(logSpeed(attempt)).toBe(24);
    expect(logSpeed({ ...attempt, stream: false })).toBe(24);
    expect(logSpeed({ ...attempt, durationMs: 500 })).toBe(120);
    expect(logSpeed({ ...attempt, outputReported: false })).toBeNull();
    expect(
      logSpeed({
        ...attempt,
        usageStatus: "session_supplemented",
        parseReasons: ["prompt_cache_total_mismatch"],
      }),
    ).toBe(24);
    expect(cacheHitRate(tokens)).toBeCloseTo(100 / 3);
    expect(cacheHitRate({ ...tokens, inclusiveInput: 0 })).toBeNull();
    expect(
      cacheHitRate({ ...tokens, inclusiveInput: null, cacheWrite: null }),
    ).toBeNull();
    const end = {
      cause: "client_disconnected",
      responseStarted: true,
      terminalReceived: false,
    } as const;
    expect(interruption({ ...attempt, end, outputReported: false })).toBe(
      "partial",
    );
    expect(interruption({ ...attempt, end, outputReported: true })).toBe(
      "reported",
    );
    expect(
      interruption({
        ...attempt,
        end,
        outputReported: true,
        usageStatus: "session_supplemented",
        parseReasons: ["prompt_cache_total_mismatch"],
      }),
    ).toBe("partial");
  });
  it("persists only an explicit visibility change and locks time", () => {
    const save = vi.fn();
    const settings: UsageSettings = {
      ...defaultSettings,
      logColumns: {},
      logNumberMode: "compact",
    };
    render(
      <UsageLogControls
        settings={settings}
        save={save}
        status=""
        onStatusChange={vi.fn()}
      />,
    );
    expect(save).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("列表设置"));
    expect(screen.getByRole("checkbox", { name: "结束时间" })).toBeDisabled();
    fireEvent.click(screen.getByRole("checkbox", { name: "推理" }));
    expect(save).toHaveBeenCalledWith({
      ...settings,
      logColumns: { reasoning: true },
    });
  });
  it("disables settings changes until settings are loaded", () => {
    const save = vi.fn();
    render(
      <UsageLogControls
        settings={undefined}
        save={save}
        status=""
        onStatusChange={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByText("列表设置"));
    expect(
      screen.getByRole("combobox", { name: "日志数字格式" }),
    ).toBeDisabled();
    for (const checkbox of screen.getAllByRole("checkbox")) {
      expect(checkbox).toBeDisabled();
    }
    expect(screen.getByRole("button", { name: "恢复默认列" })).toBeDisabled();
    expect(save).not.toHaveBeenCalled();
  });
  it("opens details by keyboard and retains grouped columns", () => {
    const open = vi.fn();
    render(
      <UsageLogTable
        rows={[
          {
            id: "fixture",
            client: "dsh",
            source: "proxy",
            startedAt: 1,
            attempts: [attempt],
          } as UsageRecord,
        ]}
        busy={false}
        settings={defaultSettings}
        name={() => "Fixture"}
        open={open}
      />,
    );
    const row = screen.getByRole("row", { name: /请求详情/ });
    fireEvent.keyDown(row, { key: "Enter" });
    expect(open).toHaveBeenCalledOnce();
    expect(screen.getByTitle("Fixture · DSH")).toBeInTheDocument();
    expect(
      screen.getAllByRole("columnheader").map((c) => c.textContent),
    ).toEqual(["时间 / 供应商 / 模型", "Token", "费用", "性能"]);
    expect(
      screen.getByTitle("fixture-request → fixture-response"),
    ).toBeInTheDocument();
  });
  it("keeps selected optional metrics and exact long numbers in their groups", () => {
    const renderTable = (settings: UsageSettings) => (
      <UsageLogTable
        rows={[
          {
            id: "fixture-exact",
            client: "dsh",
            source: "proxy",
            startedAt: 1,
            attempts: [
              {
                ...attempt,
                tokens: { ...tokens, input: 1234567890123 },
                price: { cost: "0.000000123456", multiplier: "1" },
              },
            ],
          } as UsageRecord,
        ]}
        busy={false}
        settings={settings}
        name={() => "Fixture"}
        open={() => {}}
      />
    );
    const settings: UsageSettings = {
      ...defaultSettings,
      logNumberMode: "exact",
      logColumns: { reasoning: true, hitRate: true, status: true },
    };
    const { rerender } = render(renderTable(settings));
    const row = screen.getByRole("row", { name: /请求详情/ });
    expect(within(row).getByText("1,234,567,890,123")).toBeInTheDocument();
    expect(within(row).getByText("$0.000000123456")).toBeInTheDocument();
    expect(within(row).getByLabelText("推理")).toHaveTextContent("20");
    expect(within(row).getByLabelText("HTTP")).toHaveTextContent("200");
    expect(within(row).getByLabelText("命中率")).toBeInTheDocument();
    for (const label of [
      "输入",
      "输出",
      "缓存读",
      "缓存写",
      "推理",
      "命中率",
    ]) {
      const metric = within(row).getByLabelText(label);
      expect(metric.querySelector("svg")).toHaveAttribute(
        "aria-hidden",
        "true",
      );
      expect(within(metric).queryByText(label)).not.toBeInTheDocument();
    }
    rerender(
      renderTable({
        ...settings,
        logColumns: {
          provider: false,
          model: false,
          input: false,
          output: false,
          cacheRead: false,
          cacheWrite: false,
          cost: false,
          firstToken: false,
          duration: false,
          speed: false,
        },
      }),
    );
    expect(
      screen.getAllByRole("columnheader").map((cell) => cell.textContent),
    ).toEqual(["时间 / 供应商 / 模型"]);
    expect(within(row).queryByText("$0.000000123456")).not.toBeInTheDocument();
  });
  it.each([
    [0, 0, "good", "good"],
    [10000, 60000, "warn", "warn"],
    [30000, 180000, "slow", "slow"],
    [60000, 300000, "critical", "critical"],
  ] as const)(
    "colors measured first-token %i ms and duration %i ms without coloring their labels",
    (firstTokenMs, durationMs, firstSeverity, durationSeverity) => {
      render(
        <UsageLogTable
          rows={[
            {
              id: "fixture-latency",
              client: "codex",
              source: "proxy",
              startedAt: 1,
              attempts: [{ ...attempt, firstTokenMs, durationMs }],
            } as UsageRecord,
          ]}
          busy={false}
          settings={defaultSettings}
          name={() => "Fixture"}
          open={() => {}}
        />,
      );
      for (const [label, severity] of [
        ["首字", firstSeverity],
        ["总耗时", durationSeverity],
      ]) {
        const metric = screen.getByLabelText(label);
        const value = metric.querySelector(".usage-metric-value")!;
        expect(value).toHaveClass(`latency-${severity}`);
        expect(value).not.toHaveClass("is-empty");
        expect(metric.querySelector("small")).not.toHaveAttribute("class");
      }
      const speed = screen
        .getByLabelText("输出 TPS")
        .querySelector(".usage-metric-value")!;
      expect(speed).toHaveClass(durationMs === 0 ? "is-empty" : "tps");
      expect(speed.className).not.toContain("latency-");
    },
  );
  it.each([
    ["proxy", { firstTokenMs: null }, ["首字"]],
    ["proxy", { stream: false }, ["首字"]],
    ["grok", {}, ["首字", "总耗时", "输出 TPS"]],
  ] as const)(
    "keeps missing or inapplicable metrics muted for %s with %j",
    (source, overrides, labels) => {
      render(
        <UsageLogTable
          rows={[
            {
              id: "fixture-missing-latency",
              client: "grok",
              source,
              startedAt: 1,
              attempts: [{ ...attempt, ...overrides }],
            } as UsageRecord,
          ]}
          busy={false}
          settings={defaultSettings}
          name={() => "Fixture"}
          open={() => {}}
        />,
      );
      for (const label of labels) {
        const value = screen
          .getByLabelText(label)
          .querySelector(".usage-metric-value")!;
        expect(value).toHaveTextContent("—");
        expect(value).toHaveClass("is-empty");
        expect(value.className).not.toContain("latency-");
        expect(value).not.toHaveClass("tps");
      }
    },
  );
});

it("matches output tokens over total request time without a TTFT dependency", () => {
  for (const firstTokenMs of [null, 10000, 80000]) {
    expect(
      logSpeed({
        ...attempt,
        tokens: { ...tokens, input: null, output: 120 },
        durationMs: 70000,
        firstTokenMs,
      })?.toFixed(1),
    ).toBe("1.7");
  }
  for (const output of [null, 0, -1, NaN, Infinity]) {
    expect(logSpeed({ ...attempt, tokens: { ...tokens, output } })).toBeNull();
  }
  for (const durationMs of [0, -1, NaN, Infinity]) {
    expect(logSpeed({ ...attempt, durationMs })).toBeNull();
  }
  expect(
    logSpeed({ ...attempt, tokens: { ...tokens, imageOutput: 10 } }),
  ).toBeNull();
});
it("classifies mapped and mismatched models", () => {
  const relation = (
    requestedModel: string | null,
    upstreamModel: string | null,
    responseModel: string | null,
  ) => {
    const { mapped, mismatch } = modelRelation({
      requestedModel,
      upstreamModel,
      responseModel,
    });
    return [mapped, mismatch];
  };
  expect(
    relation(
      "claude-fable-5-2",
      "deepseek-v4.1-flash-1017",
      "deepseek-v4.1-flash-1017",
    ),
  ).toEqual([true, false]);
  expect(relation("fixture-a", null, "fixture-b")).toEqual([false, true]);
  expect(relation("fixture-a", "fixture-b", "fixture-a")).toEqual([true, true]);
  expect(relation("fixture-a", "fixture-a", "fixture-a")).toEqual([
    false,
    false,
  ]);
  expect(relation("fixture-a", null, null)).toEqual([false, false]);
  expect(relation(null, null, "fixture-a")).toEqual([false, false]);
  for (const [sent, received] of [
    ["Fixture-A", "models/fixture-a"],
    ["vendor/fixture-a", "fixture-a-20260102"],
    ["fixture-a-latest", "fixture-a@20260102"],
    ["fixture-a", "fixture-a-2026-01-02"],
  ])
    expect(relation(sent, null, received)).toEqual([false, false]);
});
it("stacks the mapped and returned models under the model name", () => {
  render(
    <UsageLogTable
      rows={[
        {
          id: "fixture-relation",
          client: "claude",
          source: "proxy",
          startedAt: 1,
          attempts: [
            {
              ...attempt,
              requestedModel: "fixture-request",
              upstreamModel: "fixture-mapped",
              responseModel: "fixture-other",
            },
          ],
        } as UsageRecord,
      ]}
      busy={false}
      settings={defaultSettings}
      name={() => "Fixture"}
      open={() => {}}
    />,
  );
  const title =
    "请求模型 fixture-request · 上游请求模型 fixture-mapped · 收到的响应模型 fixture-other";
  const name = screen.getByText("fixture-request");
  expect(name.tagName).toBe("STRONG");
  const lines = [...name.parentElement!.children].map((e) => e.textContent);
  expect(lines).toEqual([
    "fixture-request",
    "↳ 映射 fixture-mapped",
    "↳ 返回 fixture-other",
  ]);
  expect(screen.getByText("↳ 映射 fixture-mapped")).toHaveAttribute(
    "title",
    title,
  );
  expect(screen.getByText("↳ 返回 fixture-other")).toHaveAttribute(
    "title",
    title,
  );
  expect(screen.getByText("↳ 返回 fixture-other")).toHaveClass("mismatch");
  expect(screen.queryByText("映射")).toBeNull();
  expect(screen.queryByText("响应不一致")).toBeNull();
});
