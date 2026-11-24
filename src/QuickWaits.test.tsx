import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { mock } from "./test-bridge";
vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);
import QuickWaits from "./QuickWaits";
import { gatewayDemo } from "./gateway-preview";
beforeEach(() => {
  mock.command.mockReset();
  vi.spyOn(Date, "now").mockReturnValue(Date.now());
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});
it("keeps long compaction cancellable and shows this attempt's time without a countdown", async () => {
  const start = Math.floor(Date.now() / 1000);
  mock.command.mockResolvedValueOnce("resent");
  render(
    <QuickWaits
      gateway={{
        ...gatewayDemo,
        pendingRequests: [
          {
            requestId: "fixture-compaction",
            providerId: null,
            model: "fixture-model",
            startedAt: start - 601,
            elapsedSeconds: 601,
            attemptSeconds: 45,
            remainingSeconds: null,
            phase: "compaction",
          },
        ],
      }}
      clientId="codex"
      visible
      notify={vi.fn()}
      error={vi.fn()}
    />,
  );
  expect(screen.getByText("压缩中")).toBeInTheDocument();
  expect(screen.getByText("本次 45 秒")).toBeInTheDocument();
  expect(screen.queryByText(/剩余|已用|601/)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "重发" }));
  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith("cancel_gateway_wait", {
      clientId: "codex",
      requestId: "fixture-compaction",
    }),
  );
});
it("shows only long waits and cancels the selected request on its platform", async () => {
  const start = Math.floor(Date.now() / 1000);
  const gateway = {
    ...gatewayDemo,
    pendingRequests: [59, 60, 90].map((seconds, i) => ({
      requestId: `fixture-${i}`,
      providerId: "primary",
      model: `model-${i}`,
      startedAt: start - seconds,
      elapsedSeconds: seconds,
      // The total decides what is listed, however young the attempt is.
      attemptSeconds: 5,
      remainingSeconds: 300 - seconds,
      phase: i === 1 ? "ws_wait" : "headers",
    })),
  };
  mock.command.mockResolvedValueOnce("gone");
  const notify = vi.fn();
  const { rerender } = render(
    <QuickWaits
      gateway={gateway}
      clientId="claude"
      visible
      notify={notify}
      error={vi.fn()}
    />,
  );
  expect(screen.queryByText("model-0")).not.toBeInTheDocument();
  expect(screen.getByText("等待重连")).toBeInTheDocument();
  const row = screen
    .getByText("model-2")
    .closest(".quick-wait-row") as HTMLElement;
  fireEvent.click(within(row).getByRole("button", { name: "重发" }));
  await waitFor(() => expect(notify).toHaveBeenCalledWith("等待已结束"));
  expect(mock.command).toHaveBeenCalledWith("cancel_gateway_wait", {
    clientId: "claude",
    requestId: "fixture-2",
  });
  mock.command.mockResolvedValueOnce("resent");
  fireEvent.click(within(row).getByRole("button", { name: "重发" }));
  await waitFor(() => expect(notify).toHaveBeenCalledWith("已打断并重发"));
  mock.command.mockResolvedValueOnce("ended");
  fireEvent.click(within(row).getByRole("button", { name: "重发" }));
  await waitFor(() =>
    expect(notify).toHaveBeenCalledWith(
      "回应已开始交付，已中止，需由客户端重试",
    ),
  );
  rerender(
    <QuickWaits
      gateway={{ ...gateway, pendingRequests: [] }}
      clientId="claude"
      visible
      notify={notify}
      error={vi.fn()}
    />,
  );
  expect(screen.queryByRole("region")).not.toBeInTheDocument();
});

it("anchors the countdown to the received budget and excludes frozen time", () => {
  vi.useFakeTimers();
  let now = 10_000_000;
  vi.spyOn(Date, "now").mockImplementation(() => now);
  const pending = {
    requestId: "fixture-queued",
    providerId: null,
    model: "fixture-model",
    startedAt: 1,
    elapsedSeconds: 61,
    attemptSeconds: 20,
    remainingSeconds: 239 as number | null,
    phase: "admission",
  };
  const props = {
    clientId: "codex" as const,
    visible: true,
    notify: vi.fn(),
    error: vi.fn(),
  };
  const view = render(
    <QuickWaits
      {...props}
      gateway={{ ...gatewayDemo, pendingRequests: [pending] }}
    />,
  );
  expect(screen.getByText("等待准入")).toBeInTheDocument();
  expect(screen.getByText("本次 20 秒 · 剩余 239 秒")).toBeInTheDocument();
  act(() => {
    now += 10_000;
    vi.advanceTimersByTime(10_000);
  });
  expect(screen.getByText("本次 30 秒 · 剩余 229 秒")).toBeInTheDocument();
  view.rerender(
    <QuickWaits
      {...props}
      gateway={{
        ...gatewayDemo,
        pendingRequests: [
          {
            ...pending,
            phase: "frozen",
            elapsedSeconds: 71,
            attemptSeconds: 30,
            remainingSeconds: null,
          },
        ],
      }}
    />,
  );
  act(() => {
    now += 600_000;
    vi.advanceTimersByTime(600_000);
  });
  expect(screen.getByText("本次 30 秒")).toBeInTheDocument();
  view.rerender(
    <QuickWaits
      {...props}
      gateway={{
        ...gatewayDemo,
        pendingRequests: [
          { ...pending, phase: "held", remainingSeconds: null },
        ],
      }}
    />,
  );
  expect(screen.getByText("断网暂存")).toBeInTheDocument();
  view.rerender(
    <QuickWaits
      {...props}
      gateway={{
        ...gatewayDemo,
        pendingRequests: [
          {
            ...pending,
            elapsedSeconds: 71,
            attemptSeconds: 30,
            remainingSeconds: 229,
          },
        ],
      }}
    />,
  );
  expect(screen.getByText("本次 30 秒 · 剩余 229 秒")).toBeInTheDocument();
  act(() => {
    now += 1_000;
    vi.advanceTimersByTime(1_000);
  });
  expect(screen.getByText("本次 31 秒 · 剩余 228 秒")).toBeInTheDocument();
});

it("maps buffering phase to 缓冲响应 with this attempt's seconds and cancellation", async () => {
  const start = Math.floor(Date.now() / 1000);
  mock.command.mockResolvedValueOnce("resent");
  render(
    <QuickWaits
      gateway={{
        ...gatewayDemo,
        pendingRequests: [
          {
            requestId: "fixture-buffering",
            providerId: "primary",
            model: "claude-opus-4-6",
            startedAt: start - 75,
            elapsedSeconds: 75,
            attemptSeconds: 12,
            remainingSeconds: null,
            phase: "buffering",
          },
        ],
      }}
      clientId="claude"
      visible
      notify={vi.fn()}
      error={vi.fn()}
    />,
  );
  expect(screen.getByText("缓冲响应")).toBeInTheDocument();
  expect(screen.getByText("本次 12 秒")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "重发" }));
  await waitFor(() =>
    expect(mock.command).toHaveBeenCalledWith("cancel_gateway_wait", {
      clientId: "claude",
      requestId: "fixture-buffering",
    }),
  );
});

it("adds what a receiving request has taken in, with its rate or its silence", () => {
  const start = Math.floor(Date.now() / 1000);
  const request = {
    providerId: "primary",
    model: null,
    startedAt: start - 90,
    elapsedSeconds: 90,
    attemptSeconds: 90,
    remainingSeconds: null,
    phase: "buffering",
  };
  render(
    <QuickWaits
      gateway={{
        ...gatewayDemo,
        pendingRequests: [
          {
            ...request,
            requestId: "fixture-flowing",
            receivedBytes: 735_846,
            bytesPerSecond: 1_638,
            silentSeconds: 4,
          },
          {
            ...request,
            requestId: "fixture-silent",
            receivedBytes: 512,
            bytesPerSecond: 0,
            silentSeconds: 45,
          },
          { ...request, requestId: "fixture-waiting", phase: "retry" },
        ],
      }}
      clientId="claude"
      visible
      notify={vi.fn()}
      error={vi.fn()}
    />,
  );
  const rows = [...document.querySelectorAll<HTMLElement>(".quick-wait-row")];
  expect(rows).toHaveLength(3);
  expect(within(rows[0]).getByText("已收 718.6 KB")).toBeInTheDocument();
  expect(within(rows[0]).getByText("1.6 KB/s")).toBeInTheDocument();
  expect(within(rows[1]).getByText("已收 512 B")).toBeInTheDocument();
  expect(within(rows[1]).getByText("静默 45 秒")).toBeInTheDocument();
  expect(rows[2].querySelector(".quick-wait-transfer")).toBeNull();
});
