import { render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import type { Attempt, UsageRecord } from "./usage-types";
import Detail from "./UsageDetail";
import { usageAttempt, usageRecord } from "./test-fixtures";

const startedAt = Date.UTC(2026, 9, 18, 2);

function attempt(): Attempt {
  return usageAttempt({
    id: "fixture-v12-detail-attempt",
    requestedModel: "fixture-model",
    responseModel: "fixture-model",
    pricingModel: "fixture-model",
    responseId: "fixture-response",
    startedAt,
    durationMs: 100,
    firstTokenMs: 10,
    tokens: { input: 10, output: 2, cacheRead: null, cacheWrite: null },
  });
}

function record(detailArchived?: boolean): UsageRecord {
  return usageRecord({
    ...(detailArchived === undefined ? {} : { detailArchived }),
    id: "fixture-v12-detail",
    startedAt,
    finishedAt: startedAt + 100,
    attempts: [attempt()],
  });
}

it("shows the archived marker for a compacted usage detail", () => {
  render(<Detail record={record(true)} name={(id) => id ?? ""} onClose={vi.fn()} />);

  expect(screen.getByText("细节已归档")).toBeInTheDocument();
});

it("keeps legacy details unmarked when the archive field is absent", () => {
  render(<Detail record={record()} name={(id) => id ?? ""} onClose={vi.fn()} />);

  expect(screen.queryByText("细节已归档")).not.toBeInTheDocument();
});
