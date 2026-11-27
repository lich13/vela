export type Tokens = {
  inclusiveInput?: number | null;
  input: number | null;
  output: number | null;
  reasoning?: number | null;
  cacheRead: number | null;
  cacheWrite: number | null;
  cacheWrite5m?: number | null;
  cacheWrite1h?: number | null;
  imageInput?: number | null;
  imageCacheRead?: number | null;
  imageOutput?: number | null;
  audioInput?: number | null;
  audioOutput?: number | null;
};
type PriceSnapshot = {
  version: string;
  source: string;
  model: string;
  multiplier: string;
  rates: Record<string, string>;
  cost: string;
  basis?: {
    models_dev_context_threshold?: number | null;
    models_dev_match?: {
      providers: string[];
      model: string;
      matchedBy: string;
    };
    price_match?: { model: string; matchedBy: string };
    operation?: string;
    unit?: string;
    quantity?: number;
    cost_per_request?: string;
  };
};
export type ModelsDevState = {
  updatedAt: number | null;
  version: string | null;
  providers: number;
  models: number;
  error: string | null;
};
export type PriceComparison = {
  model: string;
  context: number;
  referenceAt: number | null;
  reason: string | null;
  fields: {
    field: string;
    weiShaw: string;
    modelsDev: string;
    deviationPercent: string | null;
    level: "equal" | "close" | "deviation" | "severe" | "zero_conflict";
  }[];
};
type RoutingEvidence = {
  input: string;
  hasToolDeclarations: boolean;
  restriction?: string | null;
  identity?: string | null;
  boundary?: string | null;
};
export type RoutingDetails = {
  decision: string;
  evidence: RoutingEvidence;
  outcome: string;
  attemptId?: string | null;
  failure?: string | null;
};
export type Attempt = {
  requestedSpeed?: "fast" | "standard" | null;
  responseSpeed?: "fast" | "standard" | null;
  rawInputTokens?: number | null;
  inclusiveInputTokens?: number | null;
  inputBasis?: "inclusive" | "exclusive" | null;
  outputReported?: boolean | null;
  parseReasons?: string[];
  transfer?: {
    receivedAt?: number | null;
    sentAt: number;
    firstByteAt: number | null;
    terminalAt: number | null;
    terminalEvent?: string | null;
    terminalStatus?: string | null;
    incompleteReason?: string | null;
    upstreamClientRequestHash?: string | null;
    endedAt: number;
    upstreamBytes: number;
    downstreamBytes: number;
    connection?: {
      id: string;
      requestSequence: number;
      readBytes: number;
      socketWrittenBytes: number;
      events: { sequence: number; at: number; kind: string }[];
    } | null;
    terminalHandedOff: boolean;
    upstreamEof: boolean;
    upstreamEnd?: string | null;
    wsCloseCode?: number | null;
    downstreamError: string | null;
    timeoutPhase: string | null;
    timeoutSeconds: number | null;
    lastProgressMs: number | null;
    clientRequestHash: string | null;
    responseHash: string | null;
  } | null;
  end?: {
    cause: string;
    terminalReceived: boolean;
    responseStarted: boolean;
  } | null;
  threadHash?: string | null;
  upstreamRequestHash?: string | null;
  routingDecision?: string | null;
  routingOutcome?: string | null;
  routingEvidence?: RoutingEvidence | null;
  pricingBasis?: string | null;
  mappingRevision?: string | null;
  repeatCount?: number;
  compactedUnpriced?: number | null;
  operation?: "model" | "web_search" | "compaction";
  compactionKind?: string | null;
  usageStatus?: string;
  availability?: Record<string, string>;
  usageSources?: Record<string, string>;
  id: string;
  provider: string | null;
  requestedModel: string | null;
  upstreamModel?: string | null;
  responseModel: string | null;
  modelDisplayName?: string | null;
  reasoningEffort?: string | null;
  apiProvider?: string | null;
  generationMs?: number | null;
  // Inferred where no duration was reported.
  estimatedDurationMs?: number | null;
  errorCode?: string | null;
  pricingModel: string | null;
  responseId: string | null;
  status: number | null;
  outcome: string;
  startedAt: number;
  durationMs: number;
  firstTokenMs: number | null;
  stream: boolean;
  transport: string;
  tokens: Tokens;
  serviceTier: string | null;
  price: PriceSnapshot | null;
};
type FailoverEvidence = {
  stage:
    | "initial"
    | "proactive_handoff"
    | "failure_fallback"
    | "cursor_pinned"
    | "unknown_owner"
    | "delegated";
  failure:
    | "rate_limit"
    | "account_rejected"
    | "model_unavailable"
    | "upstream_service"
    | "transport"
    | "protocol"
    | "unknown"
    | null;
  eligibleCandidates: number;
  exclusions: Record<string, number>;
  wait: { kind: string; seconds: number } | null;
  exit:
    | "forwarded"
    | "no_eligible_provider"
    | "attempts_exhausted"
    | "permanent_rejection"
    | "cancelled"
    | "stopped"
    | "admission_rejected"
    | "replay_unsafe"
    | string
    | null;
};
export type UsageRecord = {
  detailArchived?: boolean;
  failoverEvidence?: FailoverEvidence | null;
  finishedAt?: number | null;
  id: string;
  client: string;
  /** The id of the handed-out gateway key the request came in with, if any. */
  entryKey?: string;
  source: string;
  startedAt: number;
  sessionId: string | null;
  attempts: Attempt[];
  completed: boolean;
  duplicateOf: string | null;
  deduplication: string;
  mergedSources?: string[];
};
export type Totals = {
  requests: number;
  attempts?: number;
  success: number;
  statusKnown: number;
  sessions: number;
  tokens: Tokens;
  cost: string;
  unpriced: number;
  durationMs: number;
  measuredOutputs: number;
  generationMs: number;
  firstTokenSumMs?: number;
  firstTokenSamples?: number;
  cacheReadEligible?: number;
  cacheInputEligible?: number;
};
export type Group = { id: string; totals: Totals };
export type UsageFilter = {
  source?: "proxy" | "sessions" | "grok_sessions";
  operation?: string;
  start?: number;
  end?: number;
  client?: string;
  provider?: string;
  model?: string;
  sessionId?: string;
  status?: string;
  page?: number;
  /** A handed-out gateway key's id, or `ownEntryKey` for requests with none. */
  entryKey?: string;
};
/** Stands for the client's own gateway key where a key's id is asked or answered. */
export const ownEntryKey = "default";
/** What a key is called once it is deleted and only its usage is left. */
export const deletedKeyName = (id: string) => `Key ${id.slice(0, 8)}`;

type AntigravityTokenMix = {
  input: number;
  cacheRead: number;
  cacheWrite: number;
  output: number;
  visibleOutput: number;
  reasoning: number;
  total: number;
  thinkingSharePercent: number | null;
  cacheHitPercent: number | null;
  cacheReadEligible: number;
  cacheInputEligible: number;
  cacheSavingsUsd: string;
  reasoningReported?: boolean;
};

export type AntigravityTrackWindow = {
  period: {
    start: number;
    end: number;
    remainingPercent?: number | null;
  } | null;
  /** Why there is no period, where the account's quota said so. */
  without?: "disabled" | "noReset" | null;
  remainingPercent?: number | null;
  requests: number;
  tokens: number;
  reasoningTokens: number;
  costUsd: string;
  unpricedRequests: number;
  /** USD per hour over the period, formatted like `costUsd`; "0" without a period. */
  costPerHour: string;
};

type AntigravityQuotaTrack = {
  id: string;
  label: string;
  burst5h: AntigravityTrackWindow;
  weekly7d: AntigravityTrackWindow;
};

type AntigravityProviderGroup = {
  id: string;
  requests: number;
  tokens: number;
  reasoningTokens: number;
  reasoningReported?: boolean;
  costUsd: string;
  unpricedRequests: number;
  sharePercent: number;
};

type AntigravityFamilyGroup = {
  family: string;
  label: string;
  requests: number;
  tokens: number;
  reasoningTokens: number;
  reasoningReported?: boolean;
  costUsd: string;
  sharePercent: number;
};

export type AntigravityUsageStats = {
  requests: number;
  costUsd: string;
  unpricedRequests: number;
  tokenMix: AntigravityTokenMix;
  accounts: {
    id: string;
    name: string;
    tracks: AntigravityQuotaTrack[];
    total: AntigravityTrackWindow;
  }[];
  quotaError: string | null;
  providers?: AntigravityProviderGroup[];
  families: AntigravityFamilyGroup[];
};
export type ClaudeUsageQuota = {
  account: string | null;
  plan: string | null;
  windows: {
    id: string;
    label: string;
    usedPercent: number;
    resetsAt: number | null;
  }[];
  error: string | null;
};
export type Dashboard = {
  totals: Totals;
  providers: Group[];
  models: Group[];
  /**
   * By gateway key, `ownEntryKey` for the client's own; of every key,
   * whichever one the filter names. An older instance sends none.
   */
  entryKeys?: Group[];
  detailSince: number;
  sources: Record<string, number>;
};
export type UsagePage = {
  rows: UsageRecord[];
  total: number;
  page: number;
  detailSince: number;
};
/** Failed attempts by what the provider answered with. */
export type CurveFailures = {
  /**
   * 429, and a limit or a lack of capacity said inside a response that began
   * as any other: in its stream, or in a frame of a WebSocket.
   */
  limited: number;
  /** Any other 4xx. */
  rejected: number;
  /** 5xx. */
  server: number;
  /**
   * No error status: it could not be reached, did not answer in time, or
   * began a response and did not finish it.
   */
  broken: number;
};
/** Times to the first token in milliseconds, by nearest rank. */
export type CurveFirstToken = {
  samples: number;
  p50: number | null;
  p90: number | null;
};
export type CurveBucket = {
  at: number;
  /**
   * Requests the provider completed. In the bucket the journal begins in,
   * those since it began: the ones its failures are counted beside.
   */
  ok: number;
  /** Null where the journal no longer reaches: unknown, not none. */
  failures: CurveFailures | null;
  firstToken: CurveFirstToken;
};
export type CurveProvider = {
  id: string;
  /** Requests it was the last to be asked for, over the whole range. */
  requests: number;
  /** Requests it completed in the time failures are known for. */
  ok: number;
  failures: CurveFailures;
  /** The share of attempts that failed in each stretch of the range. */
  trend: (number | null)[];
  /** Over the whole range. */
  firstToken: CurveFirstToken;
  /**
   * Until when in the range only its sums by day are left, which have no
   * time of day and no percentile to take. Null where it has none.
   */
  summedUntil: number | null;
  /** Only those in which something ended, in order of time. */
  buckets: CurveBucket[];
};
/**
 * What succeeded and how soon comes from the usage records, what failed from
 * the journal, which is kept for less time.
 */
export type UsageCurves = {
  /** Where the first bucket begins and the last one ends. */
  start: number;
  end: number;
  step: number;
  /** From when failures are known. */
  failuresSince: number;
  /**
   * What is known to be wrong with the journal, so that failures may be
   * missing from it. Null where nothing is.
   */
  journalError: string | null;
  /**
   * The providers with traffic or failures, the most rate-limited first and
   * each with its buckets: choosing among them asks nothing more.
   */
  providers: CurveProvider[];
};
export type UsageSettings = {
  logColumns?: Record<string, boolean>;
  logNumberMode?: "compact" | "exact";
  recording: boolean;
  autoSync: boolean;
  refreshSeconds: number;
  multiplier: string;
  pricingModel: "response" | "request";
};
export type UsageState = {
  settings: UsageSettings;
  syncing: boolean;
  maintenance?: string;
  reports: Record<
    string,
    {
      files: number;
      totalFiles?: number;
      merged?: number;
      pending?: number;
      historicalBefore?: number;
      phase?: string;
      imported: number;
      skipped: number;
      errors: number;
      lastError: string | null;
      completedAt: number | null;
    }
  >;
  error: string | null;
};
export type ProviderPriceMapping = {
  /** Usage names the Antigravity gateway `antigravity`; its gateway id is `agy`. */
  client: "codex" | "claude" | "antigravity" | "grok" | "dsh";
  provider: string;
  enabled: boolean;
  matchOn: "request" | "response";
  fromModel: string;
  toModel: string;
};
export type PricingConfig = {
  providerMappings?: ProviderPriceMapping[];
  autoUpdate: boolean;
  selected: string[] | null;
  excluded: string[];
  fixed: Record<string, Record<string, unknown>>;
  aliases: Record<string, string>;
};
export type PricingView = {
  config: PricingConfig;
  models: Record<string, Record<string, unknown>>;
  version: string;
  source: string;
  checkedAt: number | null;
  updatedAt: number | null;
  error: string | null;
  syncing: boolean;
  revision: string;
};
export type CalculatorModel = {
  model: string;
  rates: [string, string, string, string, string] | null;
};
export type CostCalculatorInput = {
  rates: [string, string, string, string, string];
  tokens: [string, string, string, string, string];
};
export type CostCalculation = {
  rates: [string, string, string, string, string];
  costs: [string, string, string, string, string];
  total: string;
};
type PricingSearchRow = {
  model: string;
  name: string | null;
  source: string;
  matchedModel: string;
  matchedBy: string;
  data: Record<string, unknown>;
};
export type PricingSearchPage = { rows: PricingSearchRow[]; total: number };
export const numeric = (n: number | null | undefined) =>
  n == null ? "—" : n.toLocaleString("zh-CN");
export const compact = (n: number | null | undefined) => {
  if (n == null || !Number.isFinite(n)) return "—";
  if (Math.abs(n) < 1000) return numeric(n);
  const units = ["", "K", "M", "B"];
  let power = Math.min(3, Math.floor(Math.log10(Math.abs(n)) / 3));
  let value = Number((n / 1000 ** power).toPrecision(3));
  if (Math.abs(value) >= 1000 && power < 3) {
    power++;
    value = Number((value / 1000).toPrecision(3));
  }
  return `${value}${units[power]}`;
};
export const money = (n: string | null | undefined) =>
  n == null ? "未定价" : "$" + Number(n).toFixed(4);
export const tokenTotal = (t: Tokens) =>
  [t.input, t.output, t.cacheRead, t.cacheWrite].every((n) => n == null)
    ? null
    : [t.input, t.output, t.cacheRead, t.cacheWrite].reduce<number>(
        (a, b) => a + (b ?? 0),
        0,
      );
// The app and the CLI are one platform here; rows an older version stored
// under the CLI's own client id show the same name.
export const clientName = (c: string) =>
  c === "antigravity" || c === "agy"
    ? "Antigravity"
    : c === "dsh"
        ? "DSH"
        : c === "codex"
          ? "Codex"
          : c === "claude"
            ? "Claude Code"
            : c === "grok"
              ? "Grok Build"
              : c;

export function averageFirstToken(
  t: Pick<Totals, "firstTokenSumMs" | "firstTokenSamples"> | null | undefined,
): number | null {
  return t?.firstTokenSamples
    ? (t.firstTokenSumMs ?? 0) / t.firstTokenSamples
    : null;
}
export function firstTokenLabel(ms: number | null | undefined): string {
  return ms == null ? "—" : `${(ms / 1000).toFixed(2)}s`;
}
export function firstTokenTitle(t: Totals | null | undefined): string {
  const ms = averageFirstToken(t);
  return ms == null
    ? "无首字样本"
    : `${(ms / 1000).toFixed(3)}s · ${numeric(t?.firstTokenSamples)} 个有效样本`;
}

export type BillRequest = {
  clients: string[];
  start: number;
  end: number;
  timezone: string;
  clean?: boolean;
};
export type BillExport = {
  requests: number;
  attempts: number;
  cleaned: number;
  retained: number;
  cleanupError: string | null;
};
