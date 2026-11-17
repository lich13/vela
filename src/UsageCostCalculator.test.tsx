import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import UsageCostCalculator from "./UsageCostCalculator";
import { command } from "./bridge";

vi.mock("./bridge", () => ({ command: vi.fn() }));
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

it("selects a used model, syncs 5 unit prices and ratios bidirectionally, and imports 5m/1h cache usage", async () => {
  const user = userEvent.setup();
  render(
    <UsageCostCalculator
      filter={{ start: 1000, end: 2000, client: "claude" }}
      close={() => {}}
    />,
  );
  await screen.findByRole("option", { name: "Fixture provider" });
  await screen.findByRole("option", { name: "claude-fable-5-2" });

  // Rule 1: selecting a priced model fills all 5 unit prices and ratios.
  await user.selectOptions(
    screen.getByLabelText("已使用模型"),
    "claude-fable-5-2",
  );
  expect(screen.getByLabelText("模型")).toHaveValue("claude-fable-5-2");
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(8);
  expect(screen.getByLabelText("输出单价 / 美元每百万")).toHaveValue(40);
  expect(screen.getByLabelText("缓存读取单价 / 美元每百万")).toHaveValue(0.2);
  expect(
    screen.getByLabelText("创建 5 分钟缓存单价 / 美元每百万"),
  ).toHaveValue(10);
  expect(
    screen.getByLabelText("创建 1 小时缓存单价 / 美元每百万"),
  ).toHaveValue(16);
  expect(screen.getByLabelText("模型倍率")).toHaveValue(4);
  expect(screen.getByLabelText("输出倍率")).toHaveValue(5);
  expect(screen.getByLabelText("缓存读取倍率")).toHaveValue(0.025);
  expect(screen.getByLabelText("创建 5 分钟缓存倍率")).toHaveValue(1.25);
  expect(screen.getByLabelText("创建 1 小时缓存倍率")).toHaveValue(2);

  // Unpriced model only updates model text, keeping existing prices & ratios.
  await user.selectOptions(
    screen.getByLabelText("已使用模型"),
    "claude-unpriced",
  );
  expect(screen.getByLabelText("模型")).toHaveValue("claude-unpriced");
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(8);
  expect(screen.getByLabelText("创建 1 小时缓存单价 / 美元每百万")).toHaveValue(
    16,
  );

  // Rule 3 & Rule 5: editing 1h unit price updates 1h ratio; editing 1h ratio updates 1h unit price.
  fireEvent.change(
    screen.getByLabelText("创建 1 小时缓存单价 / 美元每百万"),
    { target: { value: "10" } },
  );
  expect(screen.getByLabelText("创建 1 小时缓存倍率")).toHaveValue(1.25);
  fireEvent.change(screen.getByLabelText("创建 1 小时缓存倍率"), {
    target: { value: "2" },
  });
  expect(
    screen.getByLabelText("创建 1 小时缓存单价 / 美元每百万"),
  ).toHaveValue(16);

  // Rule 2: editing input unit price preserves the other 4 unit prices and recomputes modelRatio & sub-ratios.
  fireEvent.change(screen.getByLabelText("输入单价 / 美元每百万"), {
    target: { value: "4" },
  });
  expect(screen.getByLabelText("输出单价 / 美元每百万")).toHaveValue(40);
  expect(screen.getByLabelText("模型倍率")).toHaveValue(2);
  expect(screen.getByLabelText("输出倍率")).toHaveValue(10);

  // Rule 4: editing modelRatio recomputes all 5 unit prices while preserving sub-ratios.
  fireEvent.change(screen.getByLabelText("输出倍率"), {
    target: { value: "5" },
  });
  fireEvent.change(screen.getByLabelText("缓存读取倍率"), {
    target: { value: "0.025" },
  });
  fireEvent.change(screen.getByLabelText("创建 5 分钟缓存倍率"), {
    target: { value: "1.25" },
  });
  fireEvent.change(screen.getByLabelText("创建 1 小时缓存倍率"), {
    target: { value: "2" },
  });
  fireEvent.change(screen.getByLabelText("模型倍率"), {
    target: { value: "4" },
  });
  expect(screen.getByLabelText("输入单价 / 美元每百万")).toHaveValue(8);
  expect(screen.getByLabelText("输出单价 / 美元每百万")).toHaveValue(40);
  expect(
    screen.getByLabelText("创建 1 小时缓存单价 / 美元每百万"),
  ).toHaveValue(16);

  await user.selectOptions(screen.getByLabelText("供应商"), "fixture-provider");
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-fixture" },
  });
  await user.click(screen.getByRole("button", { name: "填入当前范围用量" }));
  await waitFor(() =>
    expect(screen.getByLabelText("创建 1 小时缓存 Tokens")).toHaveValue(241416),
  );
  expect(screen.getByLabelText("创建 5 分钟缓存 Tokens")).toHaveValue(176000);
  expect(command).toHaveBeenCalledWith("get_usage_calculator_totals", {
    filter: {
      start: 1000,
      end: 2000,
      client: "claude",
      provider: "fixture-provider",
      model: "claude-fixture",
      source: "proxy",
    },
  });

  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  expect(command).toHaveBeenCalledWith("calculate_usage_cost", {
    input: {
      basePrice: "2",
      modelRatio: "4",
      outputRatio: "5",
      cacheReadRatio: "0.025",
      cacheWrite5mRatio: "1.25",
      cacheWrite1hRatio: "2",
      groupRatio: "1",
      tokens: ["2", "1500", "16000", "176000", "241416"],
    },
  });
  expect(screen.getByText("3.862656")).toBeVisible();
  fireEvent.change(screen.getByLabelText("分组倍率"), {
    target: { value: "0.5" },
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

it("keeps manual inputs after a calculation failure and permits retry", async () => {
  const user = userEvent.setup();
  render(<UsageCostCalculator filter={{}} close={() => {}} />);
  await screen.findByRole("option", { name: "Fixture provider" });
  fireEvent.change(screen.getByLabelText("输入 Tokens"), {
    target: { value: "100" },
  });
  vi.mocked(command).mockRejectedValueOnce(
    new Error("fixture calculation failed"),
  );
  await user.click(screen.getByRole("button", { name: "计算" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "fixture calculation failed",
  );
  expect(screen.getByLabelText("输入 Tokens")).toHaveValue(100);
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$5.685872");
  expect(command).not.toHaveBeenCalledWith(
    "get_usage_calculator_totals",
    expect.anything(),
  );
});
