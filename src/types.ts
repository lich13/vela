/** `agy` is the Antigravity CLI on the gateway; `antigravity` names the app's own files and usage. */
export type ClientId = "codex" | "claude" | "agy" | "grok" | "dsh";
export type Preferences = {
  codexHome: string;
  claudeHome: string;
  grokHome: string;
  dshHome: string;
  cliPath: string;
  theme: "system" | "dark" | "light";
  quotaRefreshSeconds: number;
  systemNotifications: boolean;
};
export type Account = {
  id: string;
  name: string;
  kind: "chatgpt" | "apiKey";
  email: string | null;
  current: boolean;
  updatedAt: number;
  credentialRevision?: string;
  subscription?: {
    planType: string | null;
    label: string;
    stale: boolean;
  } | null;
};
export type ViewState = {
  accounts: Account[];
  authRevision: string;
  configRevision: string;
  currentState: "saved" | "unsaved" | "missing" | "invalid" | "gateway";
  preferences: Preferences;
  authSync: {
    state: string;
    accountId: string | null;
    message: string;
    at: number;
  } | null;
  error: string | null;
  officialMode: OfficialMode;
};
export type OfficialMode = {
  enabled: boolean;
  state: "disabled" | "enabled" | "conflict" | "unavailable" | string;
  accountId: string | null;
  error: string | null;
};
export type ConfigClientId = ClientId | "antigravity";
export type ConfigDocument = {
  clientId: ConfigClientId;
  text: string;
  revision: string;
  path: string;
  guarded: boolean;
  canRestore: boolean;
};
export type LoginState = {
  phase: string;
  mode: string;
  url: string | null;
  code: string | null;
  message: string;
  targetAccountId?: string | null;
};
export type AppError = {
  code: string;
  message: string;
  line?: number;
  column?: number;
};
export type UpdateInfo = {
  hasUpdate: boolean;
  latestVersion: string | null;
};
/** Endless mode: wait after a status listed in the provider's risk codes. */
export const DEFAULT_UPSTREAM_RETRY_SECONDS = 33;
/** Endless mode: wait after a rate limit. */
export const DEFAULT_LIMIT_RETRY_SECONDS = 33;
/** Endless mode: wait after any other retried failure. */
export const DEFAULT_NORMAL_RETRY_SECONDS = 3;
/** Endless mode: 0–10, how far load stretches the risk and limit waits. */
export const DEFAULT_RETRY_INTENSITY = 5;
/** Seconds an endless provider is sent nothing after a rate limit. */
export const DEFAULT_LIMIT_REST_SECONDS = 8;
/** Endless mode: the risk codes of a provider that has no saved list. */
export const DEFAULT_RISK_CODES: Record<ClientId, number[]> = {
  codex: [],
  claude: [520],
  agy: [],
  grok: [],
  dsh: [524],
};
export const errorOf = (e: unknown): AppError =>
  typeof e === "object" && e !== null && "message" in e
    ? (e as AppError)
    : {
        code: "UNKNOWN",
        message: typeof e === "string" ? e : "操作失败，请重新尝试",
      };

export type GatewaySettings = {
  customFailureRules: import("./failure-policy-types").FailureRule[];
  port: number;
  maxRetries: number;
  failureThreshold: number;
  transientFailureThreshold: number;
  successThreshold: number;
  cooldownSeconds: number;
  rateLimitSeconds: number;
  hardFailureCooldownSeconds: number;
  capacityRetrySeconds: number;
  websocketRetrySeconds: number;
  endpointProbeSeconds: number;
  firstByteSeconds: number;
  compactionSeconds: number;
  idleSeconds: number;
  retryBudgetSeconds: number;
  connectSeconds: number;
  queueSeconds: number;
  maxWaiting: number;
  networkProbe: boolean;
  networkProbeAttempts: number;
  /** Claude only: longest silence once a stream has carried model output. */
  stallSeconds: number;
  /** Longest silence of a started response while its endpoint is held offline. */
  holdIdleSeconds: number;
};
export type Health = {
  state: "closed" | "open" | "half_open";
  failures: number;
  retryIn: number;
  cooldownReason: string | null;
  probeInFlight: boolean;
  available: boolean;
  cause?: {
    eventId: string;
    code: string;
    upstreamReason?: string | null;
    errorBasis?: string | null;
    reason: string;
    status: number | null;
    wsCloseCode: number | null;
  } | null;
};
export type GrokProtocol =
  | "responses"
  | "chat_completions"
  | "messages"
  | "gemini_native";
export type Provider = {
  protocol?: GrokProtocol | null;
  id: string;
  name: string;
  baseUrl: string;
  extraBaseUrls: string[];
  preferredBaseUrl: string;
  endpointMeasurements: {
    baseUrl: string;
    latencyMs: number | null;
    status: number | null;
    error: string | null;
    checkedAt: number;
  }[];
  queued: boolean;
  health: Health;
  modelHealth: { model: string; health: Health }[];
  quotaVersion: string;
  hasQuotaAccessToken: boolean;
  quota: ProviderQuota | null;
  quotaChecking: boolean;
  quotaCheck: import("./limit-labels").QuotaCheck | null;
  maxConcurrency: number;
  maxRpm: number;
  activeRequests: number;
  rpmUsed: number;
  rpmRetryIn: number;
  rpmLimited: boolean;
  rpmWaitReason: string | null;
  allowedModels: string[] | null;
  dshModels: Record<string, string | null> | null;
  handoffAfterCompaction: boolean;
  takeNewThreads: boolean;
  /** Codex and Claude: this provider serves the compaction requests. */
  takeCompaction: boolean;
  /** Replaces the model a compaction request asked for; "" keeps it. */
  compactionModel: string;
  /** Replaces the reasoning effort a compaction request asked for; "" keeps it. */
  compactionEffort: string;
  /** Codex and Claude: this provider serves approvals, titles, summaries and memories. */
  takeAuxiliary: boolean;
  /** Replaces the model an auxiliary request asked for; "" keeps it. */
  auxiliaryModel: string;
  /** Replaces the reasoning effort an auxiliary request asked for; "" keeps it. */
  auxiliaryEffort: string;
  /** Claude and Grok: this provider serves the searches the client's web search tool sends. */
  takeWebSearch: boolean;
  /** Replaces the model a web search request asked for; "" keeps it. */
  webSearchModel: string;
  /** Replaces the reasoning effort a web search request asked for; "" keeps it. */
  webSearchEffort: string;
  searchOnly: boolean;
  endOnMessageStop: boolean;
  filterCountTokens: boolean;
  bufferCompleteResponse: boolean;
  endlessMode: boolean;
  upstreamRetrySeconds: number;
  limitRetrySeconds: number;
  /** Endless mode: seconds nothing is sent after a rate limit; 0 for none. */
  limitRestSeconds: number;
  normalRetrySeconds: number;
  /** Endless mode: upstream statuses retried at the risk-control interval. */
  riskCodes: number[];
  retryIntensity: number;
  /** Claude only: request model to the upstream model sent in its place. */
  modelMap: Record<string, string>;
};
export type GatewayListener = {
  port: number;
  revision: string;
  runningClients: ClientId[];
};
export type GrokModel = {
  key: string;
  model: string;
  protocol: GrokProtocol | null;
  address: string | null;
  credentialAvailable: boolean;
  error: string | null;
};
export type CurrentConnection = {
  kind: "gateway" | "provider" | "official" | "unmatched" | "unknown" | "mixed";
  id?: string;
  by?: "urlToken" | "urlOnly";
  host?: string;
  reason?: string;
  model: string | null;
  modelSource: string | null;
  warning: string | null;
  providerName?: string | null;
  address?: string | null;
  protocol?: GrokProtocol | null;
};
export type DshProfileCurrent = { profile: string; current: CurrentConnection };
export type GatewayState = {
  listener: GatewayListener;
  /** Antigravity, while its gateway runs: where the `agy` function was written. */
  agyShell?: { profiles: string[]; command: string | null } | null;
  grokManaged: boolean;
  dshProfile: {
    profile: string;
    protocol: GrokProtocol;
    model: string | null;
    makeDefault: boolean;
  } | null;
  clientId: ClientId;
  revision: string;
  running: boolean;
  address: string;
  mode: "manual" | "auto";
  selected: string | null;
  lastSuccessful: string | null;
  configRevision: string | null;
  configProvider: string | null;
  current: CurrentConnection;
  configError: string | null;
  configWarning: string | null;
  connectionMode: "bearer" | "apiKey";
  providers: Provider[];
  settings: GatewaySettings;
  activeConnections: number;
  waitingRequests: number;
  pendingRequests: {
    requestId: string;
    providerId: string | null;
    model: string | null;
    startedAt: number;
    elapsedSeconds: number;
    remainingSeconds: number | null;
    phase: string;
    receivedBytes?: number | null;
    bytesPerSecond?: number | null;
    silentSeconds?: number | null;
  }[];
  capacityRetries: {
    providerId: string;
    retryIn: number;
    existingOwner: boolean;
  }[];
  handoffWaiting: {
    providerId: string;
    state:
      | "awaiting_compaction"
      | "awaiting_request"
      | "capacity_full"
      | "claim_in_flight";
  }[];
  websocketRetries: { providerId: string; retryIn: number }[];
  transientRetries: { providerId: string; retryIn: number }[];
  error: string | null;
  recoveryPending: boolean;
  modelCatalog: boolean;
  /** Codex only: the picker lists OpenAI's catalog instead of the queue's models. */
  modelCatalogOfficial: boolean;
  /** OpenAI's catalog as downloaded: `version` is the content's short SHA-256. */
  modelCatalogOfficialStatus: {
    models: number;
    version: string;
    updatedAt: number;
  } | null;
  modelCatalogOfficialError: string | null;
  modelCatalogError: string | null;
};
export type StartupState = {
  launchOnBoot: boolean;
};

export type QuotaPlan = {
  name: string;
  remaining: number | null;
  used: number | null;
  total: number | null;
  unit: string;
  unlimited: boolean;
  resetAt: string | null;
};
export type QuotaUsage = {
  requests: number | null;
  tokens: number | null;
  cost: number | null;
};
export type ProviderQuota = {
  providerId: string;
  version: string;
  state: "idle" | "loading" | "ok" | "unsupported" | "error";
  source: "sub2api" | "newapi" | null;
  checkedAt: number | null;
  successAt: number | null;
  retryAt: number | null;
  nextRefreshAt?: number | null;
  stale: boolean;
  error: string | null;
  keyStatus: string | null;
  plans: QuotaPlan[];
  subscriptions?: {
    name: string | null;
    status: string | null;
    expiresAt: string | null;
    active: boolean | null;
    plans: QuotaPlan[];
  }[];
  expiresAt: string | null;
  expiresAtUnix: number | null;
  today: QuotaUsage | null;
  totalUsage: QuotaUsage | null;
};

export type ModelCatalog = {
  metadata?: Record<string, ModelMetadata>;
  providerId: string;
  version: string;
  models: string[];
  checkedAt: number | null;
  stale: boolean;
  error: string | null;
  retryAt: number | null;
};
export type ModelMetadata = {
  sources: string[];
  model: string;
  matchedBy: string;
  name: string | null;
  reasoning: boolean | null;
  context: number | null;
  output: number | null;
  reasoningEfforts: Record<string, string | null> | null;
};
/** Reasoning efforts weakest first; matches Rust `CODEX_EFFORTS`. */
export const EFFORT_ORDER = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];
/** Weakest first, unknown efforts after the known in their given order. */
export const sortEfforts = (efforts: string[]) => {
  const rank = (effort: string) => {
    const at = EFFORT_ORDER.indexOf(effort);
    return at < 0 ? EFFORT_ORDER.length : at;
  };
  return [...efforts].sort((a, b) => rank(a) - rank(b));
};
