import type { Attempt, Tokens } from "./usage-types";
export const date = (n: number) =>
  new Date(n).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
export const preciseDate = (n: number) =>
  new Date(n).toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3,
    hour12: false,
  });
export const seconds = (ms: number, digits: 2 | 3) =>
  `${(ms / 1000).toFixed(digits)}s`;
// 1024 to a step, one decimal from KB up: 718.6 KB.
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  let value = n / 1024;
  let unit = "KB";
  for (const next of ["MB", "GB"]) {
    if (value < 1024) break;
    value /= 1024;
    unit = next;
  }
  return `${value.toFixed(1)} ${unit}`;
}

export function promptTokens(attempts: Attempt[]): Tokens {
  const keys: (keyof Tokens)[] = [
    "inclusiveInput",
    "input",
    "output",
    "reasoning",
    "cacheRead",
    "cacheWrite",
    "cacheWrite5m",
    "cacheWrite1h",
    "imageInput",
    "imageCacheRead",
    "imageOutput",
    "audioInput",
    "audioOutput",
  ];
  return Object.fromEntries(
    keys.map((key) => {
      const values = attempts
        .map((a) => a.tokens[key])
        .filter((value): value is number => value != null);
      return [
        key,
        values.length ? values.reduce((sum, value) => sum + value, 0) : null,
      ];
    }),
  ) as Tokens;
}
export function missingReason(
  a: Attempt,
  field: string,
  source: string,
): string {
  // An Antigravity CLI session file holds no first-token time.
  if (
    field === "first_token" &&
    source === "antigravity" &&
    a.transport === "cli_file" &&
    (a.operation ?? "model") === "model"
  )
    return "CLI 未记录";
  if (
    field === "first_token" &&
    (!["proxy", "antigravity"].includes(source) ||
      !a.stream ||
      (a.operation ?? "model") !== "model")
  )
    return "不适用";
  const kind = a.availability?.[field] ?? a.usageStatus ?? "historical_missing";
  return (
    (
      {
        upstream_unreported: "上游未报告",
        partial: "上游未报告",
        reported: "上游未报告",
        ended_early: "提前结束",
        parse_incomplete: "解析不完整",
        historical_missing: "历史缺失",
        not_applicable: "不适用",
        session_supplemented: "会话补齐",
      } as Record<string, string>
    )[kind] ?? "历史缺失"
  );
}

// Folds aliases of one model together: case, `models/` and vendor prefixes,
// trailing release dates and `-latest`.
export function normalizeModelName(model: string): string {
  let name = model.trim().toLowerCase();
  name = name.slice(name.lastIndexOf("/") + 1);
  for (;;) {
    const next = name.replace(/(?:[-@]\d{8}|-\d{4}-\d{2}-\d{2}|-latest)$/, "");
    if (next === name) return name;
    name = next;
  }
}

export type ModelRelation = {
  // The upstream was asked for a different model than the client requested.
  mapped: boolean;
  // The upstream answered with a different model than the one it was sent.
  mismatch: boolean;
  title: string;
};

export function modelRelation(
  a: Pick<Attempt, "requestedModel" | "upstreamModel" | "responseModel">,
): ModelRelation {
  const sent = a.upstreamModel || a.requestedModel;
  const mapped =
    !!a.upstreamModel &&
    !!a.requestedModel &&
    a.upstreamModel !== a.requestedModel;
  const mismatch =
    !!sent &&
    !!a.responseModel &&
    normalizeModelName(sent) !== normalizeModelName(a.responseModel);
  return {
    mapped,
    mismatch,
    title: [
      `请求模型 ${a.requestedModel || "—"}`,
      `上游请求模型 ${sent || "—"}`,
      `收到的响应模型 ${a.responseModel || "—"}`,
    ].join(" · "),
  };
}
