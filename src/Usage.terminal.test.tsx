import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import type { Attempt, UsageRecord } from "./usage-types";
import {
  detailValue,
  serveUsage,
  usageAttempt,
  usageDashboard,
  usageRecord,
} from "./test-fixtures";

vi.mock("./bridge", async () => (await import("./test-bridge")).bridge);

import Usage from "./Usage";

const startedAt = Date.UTC(2026, 9, 8, 2, 10, 20, 123);

type TransferWithDiagnostics = NonNullable<Attempt["transfer"]>;

type AttemptFixture = Omit<Attempt, "transfer"> & {
  transfer: TransferWithDiagnostics | null;
};

type RecordFixture = Omit<UsageRecord, "attempts"> & {
  attempts: AttemptFixture[];
};

function transfer(
  overrides: Partial<TransferWithDiagnostics> = {},
): TransferWithDiagnostics {
  return {
    receivedAt: startedAt,
    sentAt: startedAt + 100,
    firstByteAt: startedAt + 500,
    terminalAt: startedAt + 4_000,
    endedAt: startedAt + 5_000,
    upstreamBytes: 1_024,
    downstreamBytes: 2_048,
    terminalHandedOff: true,
    upstreamEof: true,
    upstreamEnd: "EOF",
    wsCloseCode: null,
    downstreamError: null,
    timeoutPhase: "idle",
    timeoutSeconds: 600,
    lastProgressMs: 23,
    clientRequestHash: null,
    responseHash: null,
    ...overrides,
  };
}

function attempt(overrides: Partial<AttemptFixture> = {}): AttemptFixture {
  return {
    ...usageAttempt({
      id: "fixture-v0231-attempt",
      responseId: "fixture-response-id",
      pricingBasis: "response",
      startedAt,
    }),
    transfer: transfer(),
    ...overrides,
  };
}

function record(overrides: Partial<RecordFixture> = {}): RecordFixture {
  return {
    ...usageRecord({ id: "fixture-v0231-request", startedAt }),
    attempts: [attempt()],
    ...overrides,
  };
}

let records: RecordFixture[];

beforeEach(() => {
  records = [record()];
  serveUsage(
    () => records,
    () => usageDashboard(records.length),
  );
});

async function openFirstDetail() {
  render(<Usage />);
  const user = userEvent.setup();
  const row = (await screen.findByRole("table")).querySelector("tbody tr");
  if (!row) throw new Error("用量列表缺少记录");
  await user.click(row);
  return screen.findByRole("dialog", { name: "请求详情" });
}

it("distinguishes an unknown incomplete upstream terminal and shows its diagnostics", async () => {
  records = [
    record({
      id: "fixture-upstream-incomplete",
      completed: false,
      attempts: [
        attempt({
          outcome: "unknown",
          end: {
            cause: "upstream_incomplete",
            terminalReceived: true,
            responseStarted: true,
          },
          transfer: transfer({
            terminalEvent: "response.incomplete",
            terminalStatus: "incomplete",
            incompleteReason: "unrecognized",
            upstreamClientRequestHash: "0123456789abcdef0123456789abcdef",
          }),
        }),
      ],
    }),
  ];

  const dialog = await openFirstDetail();
  expect(detailValue(dialog, "结束原因")).toHaveTextContent(
    "上游未完成，原因未确认",
  );
  expect(within(dialog).queryByText("上游返回应用错误")).not.toBeInTheDocument();
  expect(within(dialog).queryByText("上游已完成")).not.toBeInTheDocument();

  expect(detailValue(dialog, "终态事件")).toHaveTextContent("response.incomplete");
  expect(detailValue(dialog, "终态状态")).toHaveTextContent("incomplete");
  expect(detailValue(dialog, "未完成原因")).toHaveTextContent("未识别");
  expect(detailValue(dialog, "未完成原因")).not.toHaveTextContent("unrecognized");
  expect(detailValue(dialog, "上游关联摘要")).toHaveTextContent(/^0123456789ab$/);
});

it("keeps a completed upstream result separate from a downstream error", async () => {
  records = [
    record({
      id: "fixture-completed-downstream-error",
      attempts: [
        attempt({
          outcome: "success",
          end: {
            cause: "completed",
            terminalReceived: true,
            responseStarted: true,
          },
          transfer: transfer({ downstreamError: "fixture-downstream-write-error" }),
        }),
      ],
    }),
  ];

  const dialog = await openFirstDetail();
  expect(detailValue(dialog, "结束原因")).toHaveTextContent("上游已完成");
  expect(detailValue(dialog, "下游错误")).toHaveTextContent(
    "fixture-downstream-write-error",
  );
  expect(within(dialog).getByText("已完成")).toBeInTheDocument();
  expect(within(dialog).queryByText("提前结束")).not.toBeInTheDocument();
  expect(within(dialog).queryByText("结果未确认")).not.toBeInTheDocument();
});

it("keeps older transfer records without optional terminal diagnostics readable", async () => {
  records = [
    record({
      id: "fixture-legacy-transfer",
      attempts: [
        attempt({
          end: null,
          transfer: transfer(),
        }),
      ],
    }),
  ];

  const dialog = await openFirstDetail();
  expect(detailValue(dialog, "结束原因")).toHaveTextContent("历史结束原因未记录");
  expect(detailValue(dialog, "上游结束")).toHaveTextContent("正常 EOF");
  expect(within(dialog).getByText("已完成")).toBeInTheDocument();
});

it("shows a known incomplete reason and omits absent optional diagnostic fields", async () => {
  records = [
    record({
      id: "fixture-known-incomplete-reason",
      completed: false,
      attempts: [
        attempt({
          outcome: "unknown",
          end: {
            cause: "upstream_incomplete",
            terminalReceived: true,
            responseStarted: true,
          },
          transfer: transfer({ incompleteReason: "max_output_tokens" }),
        }),
      ],
    }),
  ];

  const dialog = await openFirstDetail();
  expect(detailValue(dialog, "未完成原因")).toHaveTextContent("max_output_tokens");
  for (const label of ["终态事件", "终态状态", "上游关联摘要"]) {
    expect(within(dialog).queryByText(label, { selector: "dt" })).not.toBeInTheDocument();
  }
});
