import type {
  AntigravityUsageStats,
  Attempt,
  CurveProvider,
  Dashboard,
  CostCalculatorInput,
  PricingView,
  Tokens,
  Totals,
  UsageCurves,
  UsageFilter,
  UsageRecord,
  UsageSettings,
  UsageSpeed,
  UsageState,
} from "./usage-types";
import { ownEntryKey } from "./usage-types";
import { densePreview, previewKeys } from "./preview-fixtures";
const tokens: Tokens = {
  input: 1400,
  output: 860,
  cacheRead: 3200,
  cacheWrite: 0,
};
const rows: UsageRecord[] = Array.from({ length: 47 }, (_, i) => {
  const at = Date.now() - i * 1800000;
  const attempt: Attempt = {
    id: `fixture-attempt-${i}`,
    provider: ["primary", "claude-primary", "backup", "claude-backup"][i % 4],
    requestedModel: i % 2 ? "claude-example" : "gpt-example",
    responseModel: i % 2 ? "claude-example" : "gpt-example-2026",
    pricingModel: i % 2 ? "claude-example" : "gpt-example-2026",
    responseId: null,
    status: i % 5 ? 200 : 429,
    outcome: i % 5 ? "success" : "failure",
    startedAt: at,
    durationMs: 5200,
    firstTokenMs: 340,
    stream: true,
    transport: "http",
    operation: i % 8 === 2 ? "compaction" : "model",
    compactionKind: i % 8 === 2 ? "request" : null,
    usageStatus: "reported",
    tokens:
      i % 5 ? { ...tokens } : { ...tokens, input: 0, output: 0, cacheRead: 0 },
    serviceTier: null,
    price: {
      version: "fixture-price-version",
      source: "sub2api",
      model: "gpt-example",
      multiplier: "1",
      rates: {
        input_cost_per_token: "0.000003",
        output_cost_per_token: "0.000012",
      },
      cost: i % 5 ? "0.01512" : "0",
    },
  };
  return {
    id: `fixture-request-${i}`,
    client: i % 2 ? "claude" : "codex",
    source: i % 3 ? "proxy" : i % 2 ? "claude" : "codex",
    startedAt: at,
    sessionId: null,
    finishedAt: at + 5200,
    attempts: [{ ...attempt, status: i % 3 ? attempt.status : null }],
    completed: i % 5 !== 0,
    duplicateOf: null,
    deduplication: "独立记录",
  };
});
const app = rows[4];
app.client = "antigravity";
app.source = "antigravity";
app.sessionId =
  "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
app.attempts[0] = {
  ...app.attempts[0],
  provider: null,
  transport: "local_api",
  status: null,
  requestedModel: "claude-example",
  responseModel: "claude-example",
  pricingModel: "claude-example",
  pricingBasis: "estimated_import_snapshot",
  modelDisplayName: "Fixture Model",
  reasoningEffort: "high",
  apiProvider: "API_PROVIDER_FIXTURE",
  generationMs: 4860,
  availability: { duration: "reported", first_token: "reported" },
};
if (densePreview) {
  const large = rows[2].attempts[0];
  large.provider = "primary";
  large.tokens = {
    input: 1234,
    output: 1234567,
    cacheRead: 1234567890,
    cacheWrite: 0,
  };
  large.price = {
    ...large.price!,
    cost: "385.188873",
    rates: {
      input_cost_per_token: "0.000003",
      output_cost_per_token: "0.000012",
      cache_read_input_token_cost: "0.0000003",
    },
  };
}
const search = rows[1];
search.client = "codex";
search.source = "proxy";
search.attempts[0] = {
  ...search.attempts[0],
  provider: "primary",
  operation: "web_search",
  requestedModel: null,
  responseModel: null,
  pricingModel: "web_search",
  tokens: { input: null, output: null, cacheRead: null, cacheWrite: null },
  price: {
    version: "web-search-v1",
    source: "request",
    model: "web_search",
    multiplier: "1",
    rates: { cost_per_request: "0.01" },
    cost: "0.01",
    basis: {
      operation: "web_search",
      unit: "request",
      quantity: 1,
      cost_per_request: "0.01",
    },
  },
};
if (densePreview) {
  const completed = rows[2].attempts[0];
  completed.transport = "websocket";
  completed.end = {
    cause: "client_disconnected",
    terminalReceived: true,
    responseStarted: true,
  };
  completed.routingDecision = "compaction_handoff";
  completed.transfer = {
    receivedAt: completed.startedAt,
    sentAt: completed.startedAt + 100,
    firstByteAt: completed.startedAt + 300,
    terminalAt: completed.startedAt + 5100,
    terminalEvent: "response.completed",
    terminalStatus: "completed",
    endedAt: completed.startedAt + 5200,
    upstreamBytes: 1048576,
    downstreamBytes: 1048576,
    terminalHandedOff: true,
    upstreamEof: true,
    upstreamEnd: "EOF",
    downstreamError: null,
    timeoutPhase: null,
    timeoutSeconds: null,
    lastProgressMs: 0,
    clientRequestHash: null,
    responseHash: null,
  };
  const interrupted = rows[4];
  interrupted.completed = false;
  interrupted.attempts[0].outcome = "unknown";
  interrupted.attempts[0].end = {
    cause: "client_disconnected",
    terminalReceived: false,
    responseStarted: true,
  };
  interrupted.attempts[0].routingDecision = "existing_thread";
  interrupted.attempts[0].transfer = {
    receivedAt: interrupted.startedAt,
    sentAt: interrupted.startedAt + 100,
    firstByteAt: interrupted.startedAt + 300,
    terminalAt: null,
    endedAt: interrupted.startedAt + 5200,
    upstreamBytes: 16384,
    downstreamBytes: 8192,
    terminalHandedOff: false,
    upstreamEof: false,
    upstreamEnd: null,
    downstreamError: "DOWNSTREAM_CONNECTION_ERROR",
    connection: {
      id: "fixture-connection-id",
      requestSequence: 2,
      readBytes: 4096,
      socketWrittenBytes: 8448,
      events: [
        { sequence: 1, at: interrupted.startedAt + 5100, kind: "peer_reset" },
        {
          sequence: 2,
          at: interrupted.startedAt + 5200,
          kind: "response_body_cancelled",
        },
      ],
    },
    timeoutPhase: null,
    timeoutSeconds: null,
    lastProgressMs: 10,
    clientRequestHash: null,
    responseHash: null,
  };
  const stopped = rows[5];
  stopped.completed = false;
  stopped.attempts[0].status = 200;
  stopped.attempts[0].outcome = "cancelled";
  stopped.attempts[0].end = {
    cause: "gateway_stopped",
    terminalReceived: false,
    responseStarted: true,
  };
  const incomplete = rows[7];
  incomplete.client = "codex";
  incomplete.completed = false;
  incomplete.attempts[0].outcome = "unknown";
  incomplete.attempts[0].end = {
    cause: "upstream_incomplete",
    terminalReceived: true,
    responseStarted: true,
  };
  incomplete.attempts[0].transfer = {
    ...completed.transfer,
    receivedAt: incomplete.startedAt,
    sentAt: incomplete.startedAt + 100,
    firstByteAt: incomplete.startedAt + 300,
    terminalAt: incomplete.startedAt + 5100,
    endedAt: incomplete.startedAt + 5200,
    terminalEvent: "response.incomplete",
    terminalStatus: "incomplete",
    incompleteReason: "unrecognized",
    upstreamClientRequestHash: "0123456789abcdef".repeat(4),
  };
  const retried = rows[8];
  retried.attempts.unshift({
    ...structuredClone(retried.attempts[0]),
    id: "fixture-prior-attempt",
    provider: "primary",
    status: 503,
    outcome: "failure",
    startedAt: retried.startedAt - 1000,
    durationMs: 500,
    firstTokenMs: null,
    tokens: { input: null, output: null, cacheRead: null, cacheWrite: null },
    price: null,
    end: {
      cause: "upstream_rejected",
      terminalReceived: false,
      responseStarted: false,
    },
  });
}
if (densePreview) {
  // Every other gateway request came in with a key its client hands out, and
  // one with a key deleted since.
  const through = (client: string) =>
    rows
      .filter((row) => row.source === "proxy" && row.client === client)
      .filter((_, i) => i % 2 === 0);
  through("codex").forEach((row, i) => {
    row.entryKey = i ? previewKeys.codex[i % 2].id : previewKeys.deleted;
  });
  for (const row of through("claude")) row.entryKey = previewKeys.claude[0].id;
}
/** Whether a request is counted under the key a filter names. */
const underKey = (row: UsageRecord, key: string | undefined) =>
  !key || (row.entryKey ?? ownEntryKey) === key;
for (const row of rows) {
  row.finishedAt =
    row.attempts.at(-1)?.transfer?.terminalAt ??
    row.attempts.at(-1)?.transfer?.endedAt ??
    row.finishedAt;
}
let settings: UsageSettings = {
  recording: true,
  autoSync: true,
  refreshSeconds: 30,
  multiplier: "1",
  pricingModel: "response",
};
let price: PricingView = {
  config: {
    autoUpdate: true,
    selected: null,
    excluded: [],
    fixed: {},
    aliases: {},
  },
  models: {
    "gpt-example": {
      input_cost_per_token: 0.000003,
      output_cost_per_token: 0.000012,
      cache_read_input_token_cost: 0.0000003,
    },
    "claude-example": {
      input_cost_per_token: 0.000003,
      output_cost_per_token: 0.000015,
      cache_read_input_token_cost: 0.0000003,
      cache_creation_input_token_cost: 0.00000375,
    },
  },
  version: "fixture-price-version",
  source: "Sub2API · Wei-Shaw/model-price-repo",
  checkedAt: Date.now(),
  updatedAt: Date.now() - 86400000,
  error: null,
  syncing: false,
  revision: "fixture-pricing",
};
function total(items: UsageRecord[]): Totals {
  const first = items
    .filter((r) => ["proxy", "antigravity"].includes(r.source))
    .map((r) => r.attempts.at(-1)!)
    .filter(
      (a) =>
        a.stream &&
        (a.operation ?? "model") === "model" &&
        a.firstTokenMs != null,
    )
    .map((a) => a.firstTokenMs!);

  const sum = (key: keyof Tokens) => {
    const values = items
      .flatMap((row) => row.attempts.map((attempt) => attempt.tokens[key]))
      .filter((value): value is number => value != null);
    return values.length ? values.reduce((n, value) => n + value, 0) : null;
  };
  return {
    firstTokenSumMs: first.reduce((sum, v) => sum + v, 0),
    firstTokenSamples: first.length,
    cacheReadEligible: items
      .flatMap((r) => r.attempts)
      .filter(
        (a) =>
          a.tokens.input != null &&
          a.tokens.cacheRead != null &&
          a.tokens.cacheWrite != null,
      )
      .reduce((n, a) => n + a.tokens.cacheRead!, 0),
    cacheInputEligible: items
      .flatMap((r) => r.attempts)
      .filter(
        (a) =>
          a.tokens.input != null &&
          a.tokens.cacheRead != null &&
          a.tokens.cacheWrite != null,
      )
      .reduce(
        (n, a) =>
          n + a.tokens.input! + a.tokens.cacheRead! + a.tokens.cacheWrite!,
        0,
      ),
    requests: items.length,
    success: items.filter((r) => r.attempts.at(-1)!.status === 200).length,
    statusKnown: items.filter((r) => r.attempts.at(-1)!.status != null).length,
    sessions: items.filter((r) => r.source !== "proxy").length,
    tokens: {
      input: sum("input"),
      output: sum("output"),
      cacheRead: sum("cacheRead"),
      cacheWrite: sum("cacheWrite"),
    },
    cost: String(
      items.reduce(
        (n, r) => n + Number(r.attempts.at(-1)?.price?.cost ?? 0),
        0,
      ),
    ),
    unpriced: 0,
    durationMs: items.length * 5200,
    measuredOutputs: sum("output") ?? 0,
    generationMs: items.length * 4860,
  };
}
/**
 * Curves made up for the preview: one provider rate-limited in bursts, one
 * steady, two with too little traffic to say much, and two that did not pass
 * through a gateway. The two of Claude Code's gateway are of one host.
 */
function curvesPreview(f: UsageFilter): UsageCurves {
  const minute = 60_000;
  const end = f.end ?? Date.now();
  const start = f.start ?? end - 30 * 1440 * minute;
  const step =
    [5, 10, 15, 30, 60, 120, 180, 360, 720]
      .map((minutes) => minutes * minute)
      .find((step) => end - start <= 60 * step) ?? 1440 * minute;
  // Buckets begin on the hours of this machine's clock.
  const zone = new Date(end).getTimezoneOffset() * minute;
  const floor = (at: number) => Math.floor((at - zone) / step) * step + zone;
  const first = floor(start);
  const count = Math.round((floor(end) - first) / step) + 1;
  const failuresSince = end - 7 * 1440 * minute;
  // The same curve each time it is asked for.
  const noise = (seed: number) => {
    const x = Math.sin(seed * 12.9898) * 43758.5453;
    return x - Math.floor(x);
  };
  // Those without a host did not pass through a gateway; a client's sessions
  // tell no time to the first token.
  const shapes = [
    { id: "claude-primary", client: "claude", host: "claude.example.com", load: 44, limited: 0.5, wave: 11, other: 0.03, gaps: 0, slow: 2300 },
    { id: "primary", client: "codex", host: "api.example.com", load: 80, limited: 0.02, wave: 37, other: 0.02, gaps: 0, slow: 850 },
    { id: "backup", client: "codex", host: "backup.example.com", load: 7, limited: 0.16, wave: 13, other: 0.08, gaps: 0.45, slow: 1500 },
    { id: "claude-backup", client: "claude", host: "claude.example.com", load: 2, limited: 0, wave: 1, other: 0.2, gaps: 0.6, slow: 3200 },
    { id: "ag-oauth-1", client: "antigravity", host: null, load: 16, limited: 0.06, wave: 19, other: 0.05, gaps: 0.2, slow: 1900 },
    { id: "session:claude", client: "claude", host: null, load: 9, limited: 0.03, wave: 23, other: 0.06, gaps: 0.35, slow: 0 },
  ];
  const scale = step / (60 * minute);
  const kinds = ["limited", "rejected", "server", "broken"] as const;
  type Buckets = CurveProvider["buckets"];
  /** A row of the answer from its buckets. */
  const row = (id: string, buckets: Buckets): CurveProvider => {
    const failures = { limited: 0, rejected: 0, server: 0, broken: 0 };
    const stretches = Math.min(20, count);
    const trend = Array.from({ length: stretches }, () => [0, 0]);
    let ok = 0;
    for (const bucket of buckets) {
      const lost = bucket.failures;
      if (!lost) continue;
      ok += bucket.ok;
      const stretch =
        trend[
          Math.min(
            stretches - 1,
            Math.floor((((bucket.at - first) / step) * stretches) / count),
          )
        ];
      stretch[0] += bucket.ok;
      for (const kind of kinds) {
        failures[kind] += lost[kind];
        stretch[1] += lost[kind];
      }
    }
    const timed = buckets.filter((bucket) => bucket.firstToken.samples);
    const sorted = (pick: (bucket: Buckets[number]) => number) =>
      timed.map(pick).sort((a, b) => a - b);
    const middles = sorted((bucket) => bucket.firstToken.p50 ?? 0);
    const tops = sorted((bucket) => bucket.firstToken.p90 ?? 0);
    return {
      id,
      requests: buckets.reduce((sum, bucket) => sum + bucket.ok, 0),
      ok,
      failures,
      trend: trend.map(([ok, lost]) => (ok + lost ? lost / (ok + lost) : null)),
      firstToken: {
        samples: timed.reduce((sum, b) => sum + b.firstToken.samples, 0),
        p50: middles[Math.floor(middles.length / 2)] ?? null,
        p90: tops[Math.floor(tops.length * 0.9)] ?? null,
      },
      summedUntil: null,
      buckets,
    };
  };
  const rows = shapes
    .filter((shape) => !f.client || shape.client === f.client)
    .map((shape, s) => {
      const buckets: Buckets = [];
      for (let i = 0; i < count; i++) {
        const seed = i * 7 + s * 101;
        if (noise(seed) < shape.gaps) continue;
        const at = first + i * step;
        const ok = Math.max(
          1,
          Math.round(shape.load * scale * (0.35 + 1.3 * noise(seed + 1))),
        );
        // Rate limits come in bursts, and what does answer answers late.
        const burst =
          Math.max(0, Math.sin((i / shape.wave) * Math.PI * 2 + s)) ** 3;
        const lost = (share: number, n: number) =>
          Math.round(ok * shape.other * share * noise(seed + n) * 2);
        const p50 = Math.round(
          shape.slow * (0.75 + 0.5 * noise(seed + 6)) * (1 + burst),
        );
        buckets.push({
          at,
          ok,
          // The bucket the journal begins in is known from there on. What
          // the records tell of is known wherever there are records.
          failures:
            shape.host == null || at + step > failuresSince
              ? {
                  limited: Math.round(
                    ok * shape.limited * burst * (1.6 + 2 * noise(seed + 2)),
                  ),
                  rejected: lost(0.4, 3),
                  server: lost(1, 4),
                  broken: lost(0.6, 5),
                }
              : null,
          firstToken: shape.slow
            ? {
                samples: ok,
                p50,
                p90: Math.round(p50 * (1.7 + noise(seed + 7))),
              }
            : { samples: 0, p50: null, p90: null },
        });
      }
      return { ...row(shape.id, buckets), host: shape.host };
    });
  const order = (a: CurveProvider, b: CurveProvider) =>
    b.failures.limited - a.failures.limited || b.requests - a.requests;
  const providers = rows
    .flatMap(({ host, ...provider }) => (host ? [{ ...provider, host }] : []))
    .sort(order);
  // The providers of one host together: what each bucket holds is summed, and
  // the busier one's times stand for both, the preview having no samples.
  const hosts = new Map<string, CurveProvider[]>();
  for (const provider of providers)
    hosts.set(provider.host, [...(hosts.get(provider.host) ?? []), provider]);
  const groups = [...hosts]
    .filter(([, members]) => members.length > 1)
    .map(([host, members]): CurveProvider => {
      const at = new Map<number, Buckets[number]>();
      for (const bucket of members.flatMap((member) => member.buckets)) {
        const sum = at.get(bucket.at);
        if (!sum) {
          at.set(bucket.at, structuredClone(bucket));
          continue;
        }
        sum.ok += bucket.ok;
        if (bucket.failures) {
          sum.failures ??= { limited: 0, rejected: 0, server: 0, broken: 0 };
          for (const kind of kinds) sum.failures[kind] += bucket.failures[kind];
        }
        const samples = sum.firstToken.samples + bucket.firstToken.samples;
        sum.firstToken =
          bucket.firstToken.samples > sum.firstToken.samples
            ? { ...bucket.firstToken, samples }
            : { ...sum.firstToken, samples };
      }
      return {
        ...row(
          `host:${host}`,
          [...at.values()].sort((a, b) => a.at - b.at),
        ),
        host,
        members: members.map((member) => member.id),
      };
    })
    .sort(order);
  const direct = rows
    .flatMap(({ host, ...provider }) =>
      host ? [] : [{ ...provider, recorded: true }],
    )
    .sort(order);
  return {
    start: first,
    end: first + count * step,
    step,
    failuresSince,
    journalError: null,
    providers,
    groups,
    direct,
  };
}
/**
 * What the preview's clients are spending at this moment: each swells and
 * falls in its own time and now and then stops, so that the dials have
 * somewhere to go between one reading and the next.
 */
function speedPreview(): UsageSpeed {
  const seconds = Date.now() / 1000;
  // The scale an instance reads a rate on: its marks close up as they climb.
  const level = (perHour: number) =>
    Math.min(1, Math.log1p(perHour / 0.25) / Math.log1p(200 / 0.25));
  const rate = (perHour: number, unpriced = false) => ({
    costPerHour: String(Number(perHour.toFixed(4))),
    level: level(perHour),
    unpriced,
  });
  // The most each spends in an hour, and how many seconds a swell of it takes.
  const swells: [string, number, number][] = [
    ["codex", 16, 47],
    ["claude", 90, 73],
    ["antigravity", 7, 37],
    ["grok", 2.4, 59],
    ["dsh", 0.8, 83],
  ];
  const spent = swells.map(([id, most, period], i) => {
    const wave = Math.sin((seconds / period + i / 5) * 2 * Math.PI);
    // Under the line it spends nothing: a fifth of the time.
    return [id, most * Math.max(0, (wave + 0.8) / 1.8) ** 2] as const;
  });
  const antigravity = spent[2][1];
  const track = (id: string, share: number) => ({
    id,
    ...rate(antigravity * share),
  });
  return {
    ...rate(spent.reduce((sum, [, perHour]) => sum + perHour, 0)),
    at: Date.now(),
    dial: [1, 5, 20, 100].map((mark) => ({
      label: String(mark),
      level: level(mark),
    })),
    clients: spent.map(([id, perHour]) => ({
      id,
      // DSH is the one whose tokens are at times not priced.
      ...rate(perHour, id === "dsh" && perHour === 0),
    })),
    accounts: [
      {
        id: "ag-oauth-1",
        tracks: [track("gemini", 0.55), track("claude_gpt", 0.3)],
      },
      {
        id: "ag-oauth-2",
        tracks: [track("gemini", 0.15), track("claude_gpt", 0)],
      },
    ],
  };
}
export const usageCommands = [
  "get_usage_state",
  "get_usage_dashboard",
  "get_usage_curves",
  "get_usage_speed",
  "get_antigravity_usage_stats",
  "refresh_antigravity_usage_quota",
  "get_claude_usage_stats",
  "get_claude_usage_quota",
  "get_usage_logs",
  "get_usage_detail",
  "set_usage_settings",
  "sync_usage",
  "get_pricing",
  "calculate_usage_cost",
  "get_usage_calculator_models",
  "get_claude_requested_models",
  "get_usage_calculator_totals",
  "search_pricing",
  "configure_pricing",
  "update_pricing",
  "reload_pricing",
  "open_pricing_directory",
];
export function usagePreview(
  name: string,
  args: Record<string, unknown>,
  emit: (event: string, value: unknown) => void,
): unknown {
  const state = (): UsageState => ({
    settings: { ...settings },
    syncing: false,
    reports: {
      codex: {
        files: 8,
        imported: 16,
        skipped: 0,
        errors: 0,
        lastError: null,
        completedAt: Date.now(),
      },
      claude: {
        files: 4,
        imported: 9,
        skipped: 0,
        errors: 0,
        lastError: null,
        completedAt: Date.now(),
      },
      ...(densePreview
        ? {
            antigravity: {
              files: 52,
              imported: 98,
              skipped: 0,
              errors: 2,
              lastError:
                "Antigravity 历史用量缺少请求标识、模型或时间，请启动 app 后重新同步",
              completedAt: Date.now(),
              phase: "partial" as const,
            },
          }
        : {}),
    },
    error: null,
  });
  if (name === "set_usage_settings") {
    settings = { ...(args.settings as UsageSettings) };
    emit("usage-state", {});
    return state();
  }
  if (name === "get_usage_state" || name === "sync_usage") return state();
  if (name === "get_usage_speed") return speedPreview();
  if (name === "get_usage_detail") return rows.find((r) => r.id === args.id);
  if (name === "get_usage_calculator_totals") {
    const filter = args.filter as UsageFilter;
    const matches = rows
      .filter(
        (row) =>
          (!filter.client || row.client === filter.client) &&
          underKey(row, filter.entryKey) &&
          (!filter.start || row.startedAt >= filter.start) &&
          (!filter.end || row.startedAt <= filter.end),
      )
      .map((row) => ({
        ...row,
        attempts: row.attempts.filter(
          (a) =>
            (!filter.provider || a.provider === filter.provider) &&
            (!filter.model || a.pricingModel === filter.model),
        ),
      }))
      .filter((row) => row.attempts.length);
    return {
      ...total(matches),
      attempts: matches.reduce((n, row) => n + row.attempts.length, 0),
    };
  }
  if (name === "get_claude_requested_models") {
    const counts = new Map<string, number>();
    for (const row of rows) {
      const model = row.attempts.at(-1)?.requestedModel;
      if (row.client === "claude" && row.source === "proxy" && model)
        counts.set(model, (counts.get(model) ?? 0) + 1);
    }
    return [...counts]
      .map(([model, count]) => ({ model, count }))
      .sort((a, b) => b.count - a.count || a.model.localeCompare(b.model))
      .slice(0, 50);
  }
  if (name === "get_usage_calculator_models") {
    const client = String(args.client ?? "");
    const seen = new Set<string>();
    for (const row of rows) {
      if (client && row.client !== client) continue;
      for (const a of row.attempts) {
        const m = a.responseModel ?? a.requestedModel ?? a.pricingModel;
        if (m && m !== "unknown" && m !== "web_search") seen.add(m);
      }
    }
    return [...seen].map((model) => {
      const card = price.models[model];
      const num = (k: string) =>
        card && typeof card[k] !== "undefined" ? Number(card[k]) * 1e6 : null;
      const pIn = num("input_cost_per_token");
      const pOut = num("output_cost_per_token");
      if (pIn == null || pOut == null || !Number.isFinite(pIn) || !Number.isFinite(pOut)) {
        return { model, rates: null };
      }
      const pCr = num("cache_read_input_token_cost") ?? 0;
      const pCw5m = num("cache_creation_input_token_cost") ?? 0;
      const pCw1h = num("cache_creation_input_token_cost_above_1hr") ?? pCw5m;
      const fmt = (n: number) => String(Number(n.toFixed(8)));
      return {
        model,
        rates: [fmt(pIn), fmt(pOut), fmt(pCr), fmt(pCw5m), fmt(pCw1h)],
      };
    });
  }
  if (name === "calculate_usage_cost") {
    const input = args.input as CostCalculatorInput;
    if (
      input.rates.some(
        (v) => !String(v).trim() || Number(v) < 0 || !Number.isFinite(Number(v)),
      )
    )
      throw { code: "USAGE", message: "单价须为非负数" };
    const counts = input.tokens.map((t) => {
      const s = String(t).trim();
      if (!/^\d+$/.test(s)) throw { code: "USAGE", message: "Token 数须为有效的非负整数" };
      return Number(s);
    });
    const rates = input.rates.map(Number);
    const costs = rates.map((rate, i) => (rate * counts[i]) / 1e6);
    const format = (n: number) => String(Number(n.toFixed(12)));
    return {
      rates: rates.map(format),
      costs: costs.map(format),
      total: format(costs.reduce((a, b) => a + b, 0)),
    };
  }
  if (name === "configure_pricing") {
    price = {
      ...price,
      config: args.config as PricingView["config"],
      revision: crypto.randomUUID(),
    };
    price.models = { ...price.models, ...price.config.fixed };
    emit("pricing-state", {});
    return structuredClone(price);
  }
  if (name === "search_pricing") {
    const matches = Object.entries(price.models)
      .filter(([model]) =>
        model.toLowerCase().includes(String(args.query).toLowerCase()),
      )
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([model, data]) => ({
        model,
        name: null,
        source: model in price.config.fixed ? "fixed" : "sub2api",
        matchedModel: model,
        matchedBy: "exact",
        data,
      }));
    return {
      rows: matches.slice(
        Number(args.offset),
        Number(args.offset) + Number(args.limit),
      ),
      total: matches.length,
    };
  }
  if (name.includes("pricing"))
    return name === "open_pricing_directory"
      ? undefined
      : structuredClone(price);
  const f = (args.filter ?? {}) as UsageFilter;
  // Whatever key the filter names; the overview groups these by key.
  const everyKey = rows
    .filter(
      (r) =>
        (!f.source ||
          (f.source === "proxy"
            ? r.source === "proxy"
            : r.source !== "proxy")) &&
        (r.finishedAt ?? r.startedAt) >= (f.start ?? 0) &&
        (r.finishedAt ?? r.startedAt) <= (f.end ?? Infinity) &&
        (!f.client || r.client === f.client) &&
        (!f.provider || r.attempts.at(-1)!.provider === f.provider) &&
        (!f.model || r.attempts.at(-1)!.pricingModel === f.model) &&
        (!f.sessionId || r.sessionId === f.sessionId) &&
        (!f.operation ||
          (r.attempts.at(-1)!.operation ?? "model") === f.operation),
    )
    .sort(
      (a, b) => (b.finishedAt ?? b.startedAt) - (a.finishedAt ?? a.startedAt),
    );
  const selected = everyKey.filter((row) => underKey(row, f.entryKey));
  if (name === "get_claude_usage_quota")
    return {
      account: "f***@example.invalid",
      plan: "Max",
      error: null,
      windows: [
        {
          id: "five_hour",
          label: "当前 5h",
          usedPercent: 32.4,
          resetsAt: Date.now() + 7200000,
        },
        {
          id: "seven_day",
          label: "当前 7d",
          usedPercent: 61.7,
          resetsAt: Date.now() + 172800000,
        },
        {
          id: "seven_day_sonnet",
          label: "Sonnet · 当前 7d",
          usedPercent: 12,
          resetsAt: Date.now() + 172800000,
        },
      ],
    };
  if (name === "refresh_antigravity_usage_quota")
    return { changed: false, error: null };
  if (
    name === "get_antigravity_usage_stats" ||
    name === "get_claude_usage_stats"
  ) {
    const now = Date.now();
    const unobserved = {
      period: null,
      requests: 0,
      tokens: 0,
      reasoningTokens: 0,
      costUsd: "0",
      unpricedRequests: 0,
      costPerHour: "0",
    };
    const previewStats: AntigravityUsageStats = {
      requests: 18,
      costUsd: "1.4826",
      unpricedRequests: 0,
      tokenMix: {
        input: 64200,
        cacheRead: 298400,
        cacheWrite: 12000,
        output: 42800,
        visibleOutput: 24400,
        reasoning: 18400,
        total: 417400,
        thinkingSharePercent: 43,
        cacheHitPercent: 79.7,
        cacheReadEligible: 298400,
        cacheInputEligible: 374600,
        cacheSavingsUsd: "0.8057",
      },
      quotaError: null,
      accounts: [
        {
          id: "ag-oauth-1",
          name: "Antigravity OAuth",
          total: unobserved,
          tracks: [
            {
              id: "gemini",
              label: "Gemini",
              burst5h: {
                period: {
                  start: now - 4 * 3_600_000,
                  end: now + 3_600_000,
                  remainingPercent: 74.5,
                },
                remainingPercent: 74.5,
                requests: 7,
                tokens: 182400,
                reasoningTokens: 8200,
                costUsd: "0.492",
                unpricedRequests: 0,
                costPerHour: "0.0984",
              },
              weekly7d: {
                period: {
                  start: now - 144 * 3_600_000,
                  end: now + 24 * 3_600_000,
                  remainingPercent: 86,
                },
                remainingPercent: 86,
                requests: 12,
                tokens: 286000,
                reasoningTokens: 11800,
                costUsd: "0.784",
                unpricedRequests: 0,
                costPerHour: "0.0047",
              },
            },
            {
              id: "claude_gpt",
              label: "Claude / GPT",
              burst5h: {
                period: {
                  start: now - 3 * 3_600_000,
                  end: now + 2 * 3_600_000,
                  remainingPercent: 42,
                },
                remainingPercent: 42,
                requests: 4,
                tokens: 94200,
                reasoningTokens: 5400,
                costUsd: "0.4686",
                unpricedRequests: 0,
                costPerHour: "0.0937",
              },
              weekly7d: {
                period: {
                  start: now - 120 * 3_600_000,
                  end: now + 48 * 3_600_000,
                  remainingPercent: 68.5,
                },
                remainingPercent: 68.5,
                requests: 6,
                tokens: 131400,
                reasoningTokens: 6600,
                costUsd: "0.6986",
                unpricedRequests: 0,
                costPerHour: "0.0042",
              },
            },
          ],
        },
        {
          id: "ag-oauth-2",
          name: "Antigravity Backup",
          total: unobserved,
          tracks: [
            {
              id: "gemini",
              label: "Gemini",
              burst5h: unobserved,
              weekly7d: unobserved,
            },
            {
              id: "claude_gpt",
              label: "Claude / GPT",
              burst5h: unobserved,
              weekly7d: unobserved,
            },
          ],
        },
      ],
      families: [
        {
          family: "gemini",
          label: "Gemini",
          requests: 12,
          tokens: 286000,
          reasoningTokens: 11800,
          costUsd: "0.784",
          sharePercent: 68.5,
        },
        {
          family: "claude",
          label: "Claude",
          requests: 6,
          tokens: 131400,
          reasoningTokens: 6600,
          costUsd: "0.6986",
          sharePercent: 31.5,
        },
      ],
    };
    if (name === "get_claude_usage_stats") {
      previewStats.accounts = [];
      previewStats.tokenMix.reasoningReported = false;
      previewStats.tokenMix.thinkingSharePercent = null;
      previewStats.families.forEach((family, i) => {
        family.family = family.label = i
          ? "claude-opus-4-6"
          : "claude-sonnet-4-6";
        family.reasoningReported = false;
      });
      previewStats.providers = [
        {
          id: "session",
          requests: 18,
          tokens: 417400,
          reasoningTokens: 0,
          reasoningReported: false,
          costUsd: "1.4826",
          unpricedRequests: 0,
          sharePercent: 100,
        },
      ];
    }
    return previewStats;
  }
  if (name === "get_usage_logs") {
    const filtered = selected.filter(
      (r) =>
        !f.status ||
        (f.status === "none"
          ? r.attempts.at(-1)!.status == null
          : String(r.attempts.at(-1)!.status).startsWith(f.status[0])),
    );
    const page = f.page ?? 1;
    return {
      rows: structuredClone(filtered.slice((page - 1) * 20, page * 20)),
      total: filtered.length,
      page,
      detailSince: 0,
    };
  }
  if (name === "get_usage_curves") return curvesPreview(f);
  if (name === "get_usage_dashboard") {
    const group = (key: (r: UsageRecord) => string, of = selected) =>
      [...new Set(of.map(key))].map((id) => ({
        id,
        totals: total(of.filter((r) => key(r) === id)),
      }));
    const d: Dashboard = {
      totals: total(selected),
      providers: group((r) => r.attempts.at(-1)!.provider ?? "session"),
      models: group((r) => r.attempts.at(-1)!.pricingModel ?? "unknown"),
      entryKeys: group((r) => r.entryKey ?? ownEntryKey, everyKey),
      detailSince: 0,
      sources: Object.fromEntries(
        group((r) => r.source).map((g) => [g.id, g.totals.requests]),
      ),
    };
    return d;
  }
}
