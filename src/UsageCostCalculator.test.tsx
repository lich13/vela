import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import UsageCostCalculator from "./UsageCostCalculator";
import { command } from "./bridge";

vi.mock("./bridge", () => ({ command: vi.fn() }));
const result = {
  rates: ["8", "40", "0.2", "10"],
  costs: ["0.000016", "0.06", "0.0032", "1.76"],
  total: "1.823216",
};
beforeEach(() => {
  vi.mocked(command).mockReset();
  vi.mocked(command).mockImplementation(async (name) => {
    if (name === "get_gateway")
      return {
        providers: [{ id: "fixture-provider", name: "Fixture provider" }],
      } as never;
    if (name === "get_usage_calculator_totals")
      return {
        attempts: 1,
        tokens: {
          input: 2,
          output: 1500,
          cacheRead: 16000,
          cacheWrite: 176000,
        },
      } as never;
    if (name === "calculate_usage_cost") return result as never;
    throw new Error(`Unexpected mutation: ${name}`);
  });
});

it("imports the selected provider and model within the current time range and calculates without changing billing", async () => {
  const user = userEvent.setup();
  render(
    <UsageCostCalculator
      filter={{ start: 1000, end: 2000, client: "claude" }}
      close={() => {}}
    />,
  );
  await screen.findByRole("option", { name: "Fixture provider" });
  await user.selectOptions(screen.getByLabelText("供应商"), "fixture-provider");
  fireEvent.change(screen.getByLabelText("模型"), {
    target: { value: "claude-fixture" },
  });
  await user.click(screen.getByRole("button", { name: "填入当前范围用量" }));
  await waitFor(() =>
    expect(screen.getByLabelText("缓存创建 Tokens")).toHaveValue(176000),
  );
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
  for (const [label, value] of [
    ["模型倍率", "4"],
    ["输出倍率", "5"],
    ["缓存读取倍率", "0.025"],
    ["缓存创建倍率", "1.25"],
  ]) {
    fireEvent.change(screen.getByLabelText(label), { target: { value } });
  }
  await user.click(screen.getByRole("button", { name: "计算" }));
  await screen.findByText("$1.823216");
  expect(command).toHaveBeenCalledWith("calculate_usage_cost", {
    input: {
      basePrice: "2",
      modelRatio: "4",
      outputRatio: "5",
      cacheReadRatio: "0.025",
      cacheWriteRatio: "1.25",
      groupRatio: "1",
      tokens: [2, 1500, 16000, 176000],
    },
  });
  expect(screen.getByText("0.0032")).toBeVisible();
  fireEvent.change(screen.getByLabelText("分组倍率"), {
    target: { value: "0.5" },
  });
  expect(screen.queryByText("$1.823216")).not.toBeInTheDocument();
  expect(
    vi
      .mocked(command)
      .mock.calls.every(([name]) =>
        [
          "get_gateway",
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
  await screen.findByText("$1.823216");
  expect(command).not.toHaveBeenCalledWith(
    "get_usage_calculator_totals",
    expect.anything(),
  );
});
