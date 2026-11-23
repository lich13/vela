import type { Attempt, Tokens, UsageSettings } from "./usage-types";

export const logColumns = [
  { id: "time", label: "结束时间", width: 132, visible: true },
  { id: "provider", label: "供应商·客户端", width: 140, visible: true },
  { id: "model", label: "模型", width: 160, visible: true },
  { id: "input", label: "输入", width: 72, visible: true },
  { id: "output", label: "输出", width: 64, visible: true },
  { id: "cacheRead", label: "缓存读", width: 72, visible: true },
  { id: "cacheWrite", label: "缓存写", width: 72, visible: true },
  { id: "reasoning", label: "推理", width: 64, visible: false },
  { id: "hitRate", label: "命中率", width: 64, visible: false },
  { id: "cost", label: "费用", width: 80, visible: true },
  { id: "firstToken", label: "首字", width: 64, visible: true },
  { id: "duration", label: "耗时", width: 72, visible: true },
  { id: "speed", label: "tok/s", width: 64, visible: true },
  { id: "status", label: "状态码", width: 56, visible: false },
] as const;
export type LogColumn = (typeof logColumns)[number]["id"];
export function columnVisible(
  id: LogColumn,
  settings: UsageSettings | undefined,
) {
  if (id === "time") return true;
  return (
    settings?.logColumns?.[id] ?? logColumns.find((c) => c.id === id)!.visible
  );
}
export function logNumber(
  value: number | null | undefined,
  exact = false,
): string {
  if (value == null) return "—";
  if (exact || value < 10_000)
    return value.toLocaleString("zh-CN", { maximumFractionDigits: 20 });
  return `${(value / (value >= 1e6 ? 1e6 : 1e3)).toFixed(1)}${value >= 1e6 ? "M" : "K"}`;
}
export function logMoney(
  value: string | null | undefined,
  exact = false,
): string {
  if (value == null) return "—";
  if (exact) return `$${value}`;
  const number = Number(value);
  return number === 0 ? "$0" : `$${number.toPrecision(4)}`;
}
export function interruption(a: Attempt): "reported" | "partial" | null {
  if (
    !a.end ||
    ![
      "client_cancelled",
      "client_disconnected",
      "downstream_write_timeout",
      "downstream_transport_error",
      "gateway_stopped",
      "upstream_disconnected",
      "stream_timeout",
      "first_byte_timeout",
    ].includes(a.end.cause)
  )
    return null;
  const output =
    a.outputReported ?? (a.end.terminalReceived || (a.tokens.output ?? 0) > 0);
  return a.tokens.input != null &&
    a.tokens.output != null &&
    output &&
    a.usageStatus !== "parse_incomplete" &&
    !a.parseReasons?.length
    ? "reported"
    : "partial";
}
export const interruptionLabel = (a: Attempt) =>
  interruption(a) === "reported"
    ? "中断·已获用量"
    : interruption(a) === "partial"
      ? "中断·用量不全"
      : "";
export function cacheHitRate(t: Tokens) {
  const full =
    t.inclusiveInput ??
    (t.input != null && t.cacheRead != null && t.cacheWrite != null
      ? t.input + t.cacheRead + t.cacheWrite
      : null);
  return full != null && full > 0 && t.cacheRead != null && t.cacheRead <= full
    ? (t.cacheRead / full) * 100
    : null;
}
function logDuration(a: Attempt): number | null {
  const known = a.availability?.duration
    ? a.availability.duration === "reported"
    : !a.parseReasons?.includes("attempt_time_unreported");
  return known && Number.isFinite(a.durationMs) && a.durationMs >= 0
    ? a.durationMs
    : null;
}
export function measuredFirstToken(a: Attempt, source: string): number | null {
  return (source === "proxy" || source === "antigravity") &&
    a.stream &&
    (a.operation ?? "model") === "model"
    ? a.firstTokenMs
    : null;
}
// The output a speed can be taken from: reported text tokens.
function speedOutput(a: Attempt): number | null {
  const output = a.tokens.output;
  return output == null ||
    !Number.isFinite(output) ||
    output <= 0 ||
    (a.tokens.imageOutput ?? 0) > 0 ||
    a.price?.basis?.unit === "image" ||
    a.outputReported === false
    ? null
    : output;
}
export const generationSpeedBasis = "（输出 − 思考）÷ 生成耗时";
// Antigravity counts thinking in its output; set against the time after the
// first content that gives speeds no model reaches, so only the rest is. A
// window too short to measure, or one the wait before it dwarfs, gives none.
export function generationSpeed(a: Attempt): number | null {
  const ms = a.generationMs;
  const output = speedOutput(a);
  if (ms == null || ms < 100 || output == null) return null;
  if (ms < 1000 && !(a.firstTokenMs != null && a.firstTokenMs <= 4 * ms))
    return null;
  const speed = ((output - (a.tokens.reasoning ?? 0)) * 1000) / ms;
  return speed > 0 && speed <= 10_000 ? speed : null;
}
export const estimatedTimeNote = "按下一步开始时间估算";
const speedFloorNote = "输出不足 200 Token 或耗时不足 1 秒，不计算速度";
export type LogTiming = {
  duration: number | null;
  // No duration was reported; this one is inferred.
  estimated: boolean;
  speed: number | null;
  // Why a known duration and output give no speed.
  speedNote: string | null;
};
export function logTiming(a: Attempt, source: string): LogTiming {
  const reported = logDuration(a);
  const estimate =
    reported == null && (a.estimatedDurationMs ?? 0) > 0
      ? a.estimatedDurationMs!
      : null;
  const duration = reported ?? estimate;
  const output = speedOutput(a);
  const speed =
    output != null && duration != null && duration > 0
      ? (output * 1000) / duration
      : null;
  // Antigravity reports many short steps, whose speed says little.
  const small =
    source === "antigravity" &&
    speed != null &&
    (output! < 200 || duration! < 1000);
  return {
    duration,
    estimated: estimate != null,
    speed: small ? null : speed,
    speedNote: small ? speedFloorNote : null,
  };
}

export const unpricedReason = (a: Attempt) =>
  (a.responseSpeed ?? a.requestedSpeed) === "fast"
    ? "当前价源缺少 fast 对应价格"
    : "当前价源缺少模型或收费字段，未定价";
