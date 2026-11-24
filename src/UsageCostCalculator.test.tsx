import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import UsageCostCalculator from "./UsageCostCalculator";
import { command } from "./bridge";

vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
const result = {
  rates: ["8", "40", "0.2", "10", "16"],
  costs: ["0.000016", "0.06", "0.0032", "1.76", "3.862656"],
  total: "5.685872",
};
beforeEach(() => {
  vi.mocked(command).mockReset();
  vi.mocked(command).mockImplementation(async (name) => {
    if (name === "get_gateway")
      return {
        providers: [{ id: "fixture-provider", name: "Fixture provider" }],
      } as never;
    if (name === "get_usage_calculator_models")
      return [
        {
          model: "claude-fable-5-2",
          rates: ["8", "40", "0.2", "10", "16"],
        },
        {
          model: "claude-unpriced",
          rates: null,
        },
      ] as never;
    if (name === "get_usage_calculator_totals")
      return {
        attempts: 1,
        tokens: {
          input: 2,
          output: 1500,
          cacheRead: 16000,
          cacheWrite: 417416,
          cacheWrite5m: 176000,
          cacheWrite1h: 241416,
        },
      } as never;
    if (name === "calculate_usage_cost") return result as never;
    throw new Error(`Unexpected mutation: ${name}`);
  });
});

const ready = () =>
  waitFor(() =>
    expect(
      document.querySelector('datalist option[value="claude-fable-5-2"]'),
    ).not.toBeNull(),
  );
const called = (name: string) =>
  vi.mocked(command).mock.calls.filter(([n]) => n === name);

it("brings the five unit prices along with a used model that has a price", async () => {
  render(<UsageCostCalculator filter={{ client: "claude" }} close={() => {}} />);
  await ready();
  expect(screen.getByLabelText("供应商")).toHaveDisplayValue("全部供应商");
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-fable-5-2" },
  });
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(8);
  expect(screen.getByLabelText("输出单价 / 美元每百万")).toHaveValue(40);
  expect(screen.getByLabelText("缓存读取单价 / 美元每百万")).toHaveValue(0.2);
  expect(
    screen.getByLabelText("创建 5 分钟缓存单价 / 美元每百万"),
  ).toHaveValue(10);
  expect(
    screen.getByLabelText("创建 1 小时缓存单价 / 美元每百万"),
  ).toHaveValue(16);

  // A model without a price keeps the prices already entered.
  fireEvent.change(screen.getByLabelText("输出单价 / 美元每百万"), {
    target: { value: "24" },
  });
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-unpriced" },
  });
  expect(screen.getByLabelText("模型")).toHaveValue("claude-unpriced");
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(8);
  expect(screen.getByLabelText("输出单价 / 美元每百万")).toHaveValue(24);
  expect(screen.queryByLabelText(/倍率$|基准价格|Tokens$/)).toBeNull();
  expect(screen.queryByRole("button", { name: /填入/ })).toBeNull();
});

it("costs the usage of the chosen period at the prices entered", async () => {
  const user = userEvent.setup();
  render(
    <UsageCostCalculator
      filter={{
        start: new Date(2026, 0, 1, 0, 0, 0).getTime(),
        end: new Date(2026, 0, 1, 23, 59, 59).getTime(),
        client: "claude",
        operation: "fixture-operation",
      }}
      close={() => {}}
    />,
  );
  await screen.findByRole("option", { name: "Fixture provider" });
  await ready();
  await user.selectOptions(screen.getByLabelText("供应商"), "fixture-provider");
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-fable-5-2" },
  });
  fireEvent.change(screen.getByLabelText("开始"), {
    target: { value: "2026-01-02T03:04" },
  });
  fireEvent.change(screen.getByLabelText("结束"), {
    target: { value: "2026-01-03T06:07" },
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  expect(called("get_usage_calculator_totals")).toEqual([
    [
      "get_usage_calculator_totals",
      {
        filter: {
          start: new Date(2026, 0, 2, 3, 4, 0).getTime(),
          end: new Date(2026, 0, 3, 6, 7, 59).getTime(),
          client: "claude",
          provider: "fixture-provider",
          model: "claude-fable-5-2",
          source: "proxy",
        },
      },
    ],
  ]);
  expect(called("calculate_usage_cost")).toEqual([
    [
      "calculate_usage_cost",
      {
        input: {
          rates: ["8", "40", "0.2", "10", "16"],
          tokens: ["2", "1500", "16000", "176000", "241416"],
        },
      },
    ],
  ]);
  const cells = [...document.querySelectorAll("tbody tr")].map((row) =>
    [...row.children].map((cell) => cell.textContent),
  );
  expect(cells[4]).toEqual(["创建 1 小时缓存", "241,416", "16", "3.862656"]);
  expect(
    [...document.querySelectorAll("thead th")].map((cell) => cell.textContent),
  ).toEqual(["项目", "Tokens", "美元 / 百万", "费用 / 美元"]);

  // Any edit drops the result it no longer describes.
  fireEvent.change(screen.getByLabelText("输入单价 / 美元每百万"), {
    target: { value: "4" },
  });
  expect(screen.queryByText("$5.685872")).not.toBeInTheDocument();
  expect(
    vi
      .mocked(command)
      .mock.calls.every(([name]) =>
        [
          "get_gateway",
          "get_usage_calculator_models",
          "get_usage_calculator_totals",
          "calculate_usage_cost",
        ].includes(name),
      ),
  ).toBe(true);
});

it("defaults to today for every supplier and prices untimed cache writes at 5 minutes", async () => {
  const user = userEvent.setup();
  vi.mocked(command).mockImplementation(async (name) => {
    if (name === "get_usage_calculator_totals")
      return {
        attempts: 2,
        tokens: { input: 7, cacheWrite: 10, cacheWrite5m: 3, cacheWrite1h: 5 },
      } as never;
    if (name === "calculate_usage_cost") return result as never;
    return (name === "get_gateway" ? { providers: [] } : []) as never;
  });
  const before = Math.floor(Date.now() / 1000) * 1000;
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  const [{ filter }] = called("get_usage_calculator_totals").map(
    ([, args]) => args as { filter: Record<string, unknown> },
  );
  expect(filter).toMatchObject({ client: "claude", provider: "" });
  expect(filter.model).toBeUndefined();
  expect(filter.start).toBe(new Date(before).setHours(0, 0, 0, 0));
  expect(filter.end).toBeGreaterThanOrEqual(before);
  expect(filter.end).toBeLessThanOrEqual(Date.now());
  expect(command).toHaveBeenCalledWith("calculate_usage_cost", {
    input: {
      rates: ["2", "2", "2", "2", "2"],
      tokens: ["7", "0", "0", "5", "5"],
    },
  });
});

it("reports a period without matching gateway usage and calculates nothing", async () => {
  const user = userEvent.setup();
  vi.mocked(command).mockImplementation(async (name) =>
    name === "get_usage_calculator_totals"
      ? ({ attempts: 0, tokens: {} } as never)
      : name === "get_gateway"
        ? ({ providers: [] } as never)
        : ([] as never),
  );
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "所选周期没有符合条件的网关用量",
  );
  expect(called("calculate_usage_cost")).toEqual([]);
});

it("rejects a period that starts after it ends before asking for usage", async () => {
  const user = userEvent.setup();
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await ready();
  fireEvent.change(screen.getByLabelText("开始"), {
    target: { value: "2026-01-03T00:00:00" },
  });
  fireEvent.change(screen.getByLabelText("结束"), {
    target: { value: "2026-01-02T00:00:00" },
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "开始时间不能晚于结束时间",
  );
  expect(called("get_usage_calculator_totals")).toEqual([]);

  fireEvent.change(screen.getByLabelText("结束"), {
    target: { value: "2026-01-04T00:00:00" },
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  expect(screen.queryByRole("alert")).toBeNull();
});

it("keeps the inputs after a calculation failure and permits retry", async () => {
  const user = userEvent.setup();
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await ready();
  fireEvent.change(screen.getByLabelText("输入单价 / 美元每百万"), {
    target: { value: "3" },
  });
  const usual = vi.mocked(command).getMockImplementation()!;
  vi.mocked(command).mockImplementation(async (name, ...rest) => {
    if (name === "calculate_usage_cost")
      throw new Error("fixture calculation failed");
    return usual(name, ...rest) as never;
  });
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "fixture calculation failed",
  );
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(3);
  vi.mocked(command).mockImplementation(usual);
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
});
