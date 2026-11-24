import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import type { Attempt, UsageRecord } from "./usage-types";
import {
  serveUsage,
  usageAttempt,
  usageDashboard,
  usageRecord,
} from "./test-fixtures";

vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);

import Usage from "./Usage";

const timestamp = Date.UTC(2026, 9, 8, 2);

function attempt(overrides: Partial<Attempt> = {}): Attempt {
  return usageAttempt({
    id: "fixture-v0211-attempt",
    pricingBasis: "response",
    startedAt: timestamp,
    ...overrides,
  });
}

function record(overrides: Partial<UsageRecord> = {}): UsageRecord {
  return usageRecord({
    id: "fixture-v0211-request",
    startedAt: timestamp,
    attempts: [attempt()],
    ...overrides,
  });
}

let records: UsageRecord[];

beforeEach(() => {
  records = [record()];
  serveUsage(
    () => records,
    () => usageDashboard(1),
  );
});

it("renders explicit lifecycle causes and keeps legacy proxy cancellations marked as unrecorded", async () => {
  records = [
    record({
      id: "fixture-history-cancel",
      attempts: [
        attempt({
          id: "attempt-history-cancel",
          outcome: "cancelled",
          end: null,
        }),
      ],
    }),
    record({
      id: "fixture-disconnected-unknown",
      attempts: [
        attempt({
          id: "attempt-disconnected-unknown",
          outcome: "unknown",
          end: {
            cause: "client_disconnected",
            terminalReceived: false,
            responseStarted: true,
          },
        }),
      ],
      completed: false,
    }),
    record({
      id: "fixture-disconnected-after-complete",
      attempts: [
        attempt({
          id: "attempt-disconnected-after-complete",
          end: {
            cause: "client_disconnected",
            terminalReceived: true,
            responseStarted: true,
          },
        }),
      ],
    }),
    record({
      id: "fixture-gateway-stop",
      attempts: [
        attempt({
          id: "attempt-gateway-stop",
          outcome: "cancelled",
          end: {
            cause: "gateway_stopped",
            terminalReceived: false,
            responseStarted: true,
          },
        }),
      ],
      completed: false,
    }),
    record({
      id: "fixture-unsettled",
      attempts: [
        attempt({
          id: "attempt-unsettled",
          outcome: "unknown",
          end: {
            cause: "unsettled",
            terminalReceived: false,
            responseStarted: false,
          },
        }),
      ],
      completed: false,
    }),
  ];
  const user = userEvent.setup();
  render(<Usage />);
  const table = await screen.findByRole("table");
  const rows = within(table).getAllByRole("row");
  expect(rows).toHaveLength(records.length + 1);
  expect(within(rows[1]).getByText("fixture-response")).toBeInTheDocument();

  const cases = [
    ["历史结束原因未记录"],
    ["客户端断开，完成状态未知"],
    ["上游已完成，客户端随后断开"],
    ["网关停止"],
    ["结束原因未确认"],
  ];
  for (const [label] of cases) {
    const index = cases.findIndex(([expected]) => expected === label);
    const target = within(screen.getByRole("table")).getAllByRole("row")[
      index + 1
    ];
    await user.click(target);
    const drawer = await screen.findByRole("dialog", { name: "请求详情" });
    expect(within(drawer).getAllByText(label).length).toBeGreaterThan(0);
    await user.click(within(drawer).getByRole("button", { name: "关闭详情" }));
    expect(
      screen.queryByRole("dialog", { name: "请求详情" }),
    ).not.toBeInTheDocument();
  }
});

it("explains a completed upstream response followed by a client disconnect", async () => {
  records = [
    record({
      id: "fixture-success-disconnect",
      attempts: [
        attempt({
          end: {
            cause: "client_disconnected",
            terminalReceived: true,
            responseStarted: true,
          },
        }),
      ],
    }),
  ];
  const user = userEvent.setup();
  render(<Usage />);
  await user.click(
    (await screen.findByText("fixture-response")).closest("tr")!,
  );
  const drawer = await screen.findByRole("dialog", { name: "请求详情" });
  expect(
    within(drawer).getAllByText("上游已完成，客户端随后断开").length,
  ).toBeGreaterThan(0);
  expect(
    within(drawer).queryByText("客户端断开，完成状态未知"),
  ).not.toBeInTheDocument();
});
