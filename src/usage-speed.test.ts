import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
import type { InstanceState } from "./bridge-contracts";
import { setInstanceState } from "./instance";
import { speedLabel, useUsageSpeed } from "./usage-speed";
import type { UsageSpeed } from "./usage-types";

const rate = (costPerHour: string, unpriced = false) => ({
  costPerHour,
  level: 0,
  unpriced,
});

it.each([
  ["0", "$0.00/h"],
  ["0.004", "$0.00/h"],
  ["0.42", "$0.42/h"],
  ["9.99", "$9.99/h"],
  ["10", "$10.0/h"],
  ["12.3456", "$12.3/h"],
  ["99.94", "$99.9/h"],
  ["100", "$100/h"],
  ["123.456", "$123/h"],
])("reads %s USD an hour as %s", (costPerHour, label) => {
  expect(speedLabel(rate(costPerHour))).toBe(label);
});

it("reads what could not be priced as 未定价 and not as nothing spent", () => {
  expect(speedLabel(rate("0", true))).toBe("未定价");
});

const reading = (costPerHour: string, level: number): UsageSpeed => ({
  costPerHour,
  level,
  unpriced: false,
  at: 1_760_000_000_000,
  dial: [],
  clients: [],
  accounts: [],
});
const instances = (
  active: "local" | "m1",
  status: "connected" | "offline" = "connected",
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
      : { status, version: "0.49.0", error: null, unsupported: [] },
});

describe("useUsageSpeed", () => {
  const asked = () => mock.command.mock.calls.length;
  /** Lets the time go by, and what was answered meanwhile reach the hook. */
  const pass = (ms: number) =>
    act(async () => {
      await vi.advanceTimersByTimeAsync(ms);
    });
  beforeEach(() => {
    vi.useFakeTimers();
    mock.command.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
    act(() => setInstanceState(instances("local")));
  });

  it("reads at once and again every two seconds", async () => {
    const first = reading("1.5", 0.29);
    const second = reading("2.25", 0.34);
    mock.command.mockResolvedValueOnce(first).mockResolvedValue(second);
    const { result } = renderHook(() => useUsageSpeed(true));
    expect(mock.command.mock.calls).toEqual([["get_usage_speed"]]);
    expect(result.current).toBeNull();
    await pass(0);
    expect(result.current).toEqual(first);
    await pass(1999);
    expect(asked()).toBe(1);
    await pass(1);
    expect(asked()).toBe(2);
    expect(result.current).toEqual(second);
    await pass(4000);
    expect(asked()).toBe(4);
  });

  it("asks nothing until it is active, and stops and forgets once it no longer is", async () => {
    const first = reading("1.5", 0.29);
    mock.command.mockResolvedValue(first);
    const { result, rerender, unmount } = renderHook(
      ({ active }) => useUsageSpeed(active),
      { initialProps: { active: false } },
    );
    await pass(6000);
    expect(asked()).toBe(0);
    expect(result.current).toBeNull();

    rerender({ active: true });
    await pass(0);
    expect(asked()).toBe(1);
    expect(result.current).toEqual(first);

    rerender({ active: false });
    expect(result.current).toBeNull();
    await pass(6000);
    expect(asked()).toBe(1);

    // Asked for again, it shows nothing of before while it reads.
    mock.command.mockReturnValue(new Promise(() => {}));
    rerender({ active: true });
    expect(asked()).toBe(2);
    await pass(0);
    expect(result.current).toBeNull();

    unmount();
    await pass(6000);
    expect(asked()).toBe(2);
  });

  it("asks nothing of a server that is not connected, and reads again once it is", async () => {
    const first = reading("1.5", 0.29);
    mock.command.mockResolvedValue(first);
    act(() => setInstanceState(instances("m1")));
    const { result } = renderHook(() => useUsageSpeed(true));
    await pass(0);
    expect(asked()).toBe(1);
    expect(result.current).toEqual(first);

    act(() => setInstanceState(instances("m1", "offline")));
    expect(result.current).toBeNull();
    await pass(6000);
    expect(asked()).toBe(1);

    act(() => setInstanceState(instances("m1")));
    expect(asked()).toBe(2);
    await pass(0);
    expect(result.current).toEqual(first);
  });

  it("waits for a slow reading rather than ask again", async () => {
    const slow = reading("1.5", 0.29);
    const next = reading("3", 0.38);
    let answer!: (value: UsageSpeed) => void;
    mock.command
      .mockReturnValueOnce(new Promise<UsageSpeed>((done) => (answer = done)))
      .mockResolvedValue(next);
    const { result } = renderHook(() => useUsageSpeed(true));
    await pass(6000);
    expect(asked()).toBe(1);
    expect(result.current).toBeNull();

    await act(async () => answer(slow));
    expect(result.current).toEqual(slow);
    expect(asked()).toBe(1);
    await pass(2000);
    expect(asked()).toBe(2);
    expect(result.current).toEqual(next);
  });

  it("shows nothing where a reading fails, and reads again at the next turn", async () => {
    const first = reading("1.5", 0.29);
    const third = reading("3", 0.38);
    mock.command
      .mockResolvedValueOnce(first)
      .mockRejectedValueOnce(new Error("连接已断开"))
      .mockResolvedValue(third);
    const { result } = renderHook(() => useUsageSpeed(true));
    await pass(0);
    expect(result.current).toEqual(first);
    await pass(2000);
    expect(asked()).toBe(2);
    expect(result.current).toBeNull();
    await pass(2000);
    expect(asked()).toBe(3);
    expect(result.current).toEqual(third);
  });

  it("starts over for another instance, and drops what the one before answers late", async () => {
    const local = reading("1.5", 0.29);
    const late = reading("9", 0.54);
    const remote = reading("3", 0.38);
    let answer!: (value: UsageSpeed) => void;
    mock.command
      .mockResolvedValueOnce(local)
      .mockReturnValueOnce(new Promise<UsageSpeed>((done) => (answer = done)))
      .mockResolvedValue(remote);
    const { result } = renderHook(() => useUsageSpeed(true));
    await pass(0);
    expect(result.current).toEqual(local);
    // The second reading of this machine is still out when the server is chosen.
    await pass(2000);
    expect(asked()).toBe(2);

    act(() => setInstanceState(instances("m1")));
    expect(result.current).toBeNull();
    expect(asked()).toBe(3);
    await pass(0);
    expect(result.current).toEqual(remote);

    await act(async () => answer(late));
    expect(result.current).toEqual(remote);
  });
});
