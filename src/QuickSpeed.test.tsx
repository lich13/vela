import { act, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
import type { InstanceState, QuickPreferences } from "./bridge-contracts";
import { setInstanceState } from "./instance";
import QuickSpeed from "./QuickSpeed";
import type { UsageSpeed } from "./usage-types";

const speed: UsageSpeed = {
  costPerHour: "18.1",
  level: 0.64,
  unpriced: false,
  at: 1_760_000_000_000,
  dial: [
    { label: "1", level: 0.24 },
    { label: "5", level: 0.46 },
    { label: "20", level: 0.66 },
    { label: "100", level: 0.9 },
  ],
  clients: [
    { id: "codex", costPerHour: "1.5", level: 0.29, unpriced: false },
    { id: "claude", costPerHour: "12.3456", level: 0.59, unpriced: false },
    { id: "antigravity", costPerHour: "4.2544", level: 0.43, unpriced: false },
    { id: "grok", costPerHour: "0", level: 0, unpriced: true },
    { id: "dsh", costPerHour: "0", level: 0, unpriced: false },
  ],
  accounts: [],
};
const local: InstanceState = {
  revision: "r1",
  active: "local",
  machines: [],
  connection: { status: "local", version: null, error: null, unsupported: [] },
};
let quick: QuickPreferences;
const asked = () =>
  mock.command.mock.calls.filter(([name]) => name === "get_usage_speed").length;
/** The small dials, in the order they stand in. */
const platforms = () =>
  screen
    .getAllByRole("meter")
    .map((meter) => meter.getAttribute("aria-label"))
    .filter((name) => name !== "总速率");
/** Lets the time go by, and what was answered meanwhile reach the card. */
const pass = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });

beforeEach(() => {
  vi.useFakeTimers();
  quick = { pinned: false, visible: true };
  mock.listeners.clear();
  mock.command.mockReset().mockImplementation(async (name: string) => {
    if (name === "get_quick") return quick;
    if (name === "get_usage_speed") return speed;
    throw new Error(`unexpected command: ${name}`);
  });
});
afterEach(() => {
  vi.useRealTimers();
  act(() => setInstanceState(local));
});

it("reads the total on the large dial and each platform on its own, by the names the panel's tabs go by", async () => {
  const { container } = render(<QuickSpeed />);
  // Not there until something is read.
  expect(container).toBeEmptyDOMElement();
  await pass(0);

  const card = screen.getByRole("region", { name: "实时速率" });
  const total = within(card).getByRole("meter", { name: "总速率" });
  expect(total).toHaveClass("is-large");
  expect(total).toHaveAttribute("aria-valuenow", "0.64");
  expect(within(total).getByText("$18.1/h")).toHaveAttribute("title", "18.1/h");
  expect(within(total).getByText("总速率")).toBeInTheDocument();
  // Only the total has the scale's marks.
  expect(card.querySelectorAll(".speedometer-mark")).toHaveLength(4);
  expect(total.querySelectorAll(".speedometer-mark")).toHaveLength(4);

  expect(platforms()).toEqual([
    "Codex 速率",
    "Claude 速率",
    "agy 速率",
    "Grok 速率",
    "DSH 速率",
  ]);
  const claude = within(card).getByRole("meter", { name: "Claude 速率" });
  expect(within(claude).getByText("Claude")).toBeInTheDocument();
  expect(within(claude).getByText("$12.3/h")).toHaveAttribute(
    "title",
    "12.3456/h",
  );
  // Usage knows the Antigravity gateway by its long name; the panel by `agy`.
  const agy = within(card).getByRole("meter", { name: "agy 速率" });
  expect(within(agy).getByText("agy")).toBeInTheDocument();
  expect(within(agy).getByText("$4.25/h")).toBeInTheDocument();
  expect(within(card).queryByText("Antigravity")).not.toBeInTheDocument();
  expect(
    within(within(card).getByRole("meter", { name: "Grok 速率" })).getByText(
      "未定价",
    ),
  ).toBeInTheDocument();
  expect(
    within(within(card).getByRole("meter", { name: "DSH 速率" })).getByText(
      "$0.00/h",
    ),
  ).toBeInTheDocument();
});

it("leaves out a platform the panel hides, while the total still counts it", async () => {
  quick = { pinned: false, visible: true, hiddenClients: ["claude", "agy"] };
  render(<QuickSpeed />);
  await pass(0);
  expect(platforms()).toEqual(["Codex 速率", "Grok 速率", "DSH 速率"]);
  expect(
    within(screen.getByRole("meter", { name: "总速率" })).getByText("$18.1/h"),
  ).toBeInTheDocument();

  // Shown again in the settings, it is back without another reading.
  const reads = asked();
  act(() =>
    mock.listeners.get("quick-state")!({
      pinned: false,
      hiddenClients: ["dsh"],
    }),
  );
  expect(platforms()).toEqual([
    "Codex 速率",
    "Claude 速率",
    "agy 速率",
    "Grok 速率",
  ]);
  expect(asked()).toBe(reads);
});

it("is not there, and asks nothing, for an instance that cannot say how fast it spends", async () => {
  act(() =>
    setInstanceState({
      revision: "r2",
      active: "m1",
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
      connection: {
        status: "connected",
        version: "0.48.0",
        error: null,
        unsupported: ["get_usage_speed"],
      },
    }),
  );
  const { container } = render(<QuickSpeed />);
  await pass(6000);
  expect(container).toBeEmptyDOMElement();
  expect(asked()).toBe(0);
});

it("reads every two seconds only while the panel is on screen", async () => {
  const { container } = render(<QuickSpeed />);
  await pass(0);
  expect(asked()).toBe(1);
  await pass(4000);
  expect(asked()).toBe(3);

  act(() => mock.listeners.get("panel-visibility")!(false));
  expect(container).toBeEmptyDOMElement();
  await pass(6000);
  expect(asked()).toBe(3);

  // Opened again, it reads at once.
  act(() => mock.listeners.get("panel-visibility")!(true));
  expect(asked()).toBe(4);
  await pass(0);
  expect(screen.getByRole("meter", { name: "总速率" })).toBeInTheDocument();
});

it("goes where a reading fails, and is back with the next one that does not", async () => {
  mock.command.mockImplementation(async (name: string) => {
    if (name === "get_quick") return quick;
    throw new Error("连接已断开");
  });
  const { container } = render(<QuickSpeed />);
  await pass(0);
  expect(container).toBeEmptyDOMElement();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();

  mock.command.mockImplementation(async (name: string) =>
    name === "get_quick" ? quick : speed,
  );
  await pass(2000);
  expect(screen.getByRole("meter", { name: "总速率" })).toBeInTheDocument();
});
