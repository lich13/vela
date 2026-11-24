import type {
  BillRequest,
  BillExport,
  CalculatorModel,
  CostCalculatorInput,
  CostCalculation,
} from "./usage-types";
// IPC contracts mirror the registered Rust commands. Credentials only cross
// the explicit reveal/login commands and are never added to shared state.
import type {
  AppError,
  ClientId,
  ConfigClientId,
  CurrentConnection,
  DshProfileCurrent,
  ConfigDocument,
  GatewaySettings,
  GatewayState,
  GatewayListener,
  GrokModel,
  GrokProtocol,
  LoginState,
  ModelCatalog,
  ModelMetadata,
  Preferences,
  ProviderQuota,
  StartupState,
  UpdateInfo,
  ViewState,
} from "./types";
import type {
  AntigravityUsageStats,
  ClaudeUsageQuota,
  Dashboard,
  ModelsDevState,
  PriceComparison,
  PricingConfig,
  PricingView,
  PricingSearchPage,
  UsageFilter,
  Totals,
  UsagePage,
  UsageRecord,
  UsageSettings,
  UsageState,
} from "./usage-types";
import type { EventRecord, EventPage } from "./EventLog";
import type { PowerState } from "./PowerSettings";
import type { PreparedTemplate } from "./TemplateChoice";

export type { QuotaCheck, LimitEvidence } from "./limit-labels";

export type DshOfficialModel = {
  id: string;
  name: string;
  contextWindow?: number;
  maxTokens?: number;
  input?: string[];
};
export type DshOfficialCatalog = {
  models: DshOfficialModel[];
  error: string | null;
};

export type QuickPreferences = {
  pinned: boolean;
  visible?: boolean;
  hiddenClients?: ClientId[];
  selectedClient?: ClientId;
};
export type NotificationState = {
  permission: string;
  error: string | null;
  delivery?: "idle" | "pending" | "accepted" | "failed";
};
export type ClaudeProfileState = {
  mode: "official" | "api";
  revision: string;
  initialized: boolean;
  conflict: string | null;
  files: { role: string; exists: boolean; revision: string }[];
  warnings: string[];
};
export type ClaudeLoginState = {
  phase: string;
  authenticated: boolean;
  error: string | null;
};
export type AntigravityAccountItem = {
  id: string;
  name: string;
  sourceType: string;
  email: string | null;
  current: boolean;
  updatedAt: number;
  credentialRevision: string;
};
export type AntigravityAuthState = {
  accounts: AntigravityAccountItem[];
  oauthPresent: boolean;
  oauthEmail: string | null;
  oauthAuthMethod: string | null;
  /** Running `agy` sessions; each writes its own login back at a token refresh. */
  cliSessions?: number;
  cliClosed?: number;
  appRestarted?: boolean;
  relaunchFailed?: boolean;
  revision: string;
};
export type ProviderTestMode =
  | "text"
  | "compact"
  | "image"
  | "video"
  | "search"
  | "tts"
  | "stt"
  | "realtime";
export type DetectionEngine = "modelTrace" | "lmDetector";
export type VerdictMode = "strict" | "relaxed";
export type DetectionPlatform = {
  engine: DetectionEngine;
  modelTraceSamples: 1 | 3;
  lmDetectorSamples: 1 | 3;
  verdictMode?: VerdictMode;
  degradedModelId?: string;
};
export type ProviderTestKind = "connection" | "fingerprint";
export type ProviderAction = "settings" | "testing" | "edit";
export type ProviderTestRequest = {
  clientId: ClientId;
  providerId: string;
  expectedRevision: string;
  model: string;
  endpoint: string | null;
  kind: ProviderTestKind;
  concurrency: number;
  prompt?: string;
  tokenizer?: boolean;
};
export type FingerprintReport = {
  prediction: string;
  prediction_name: string;
  probability: number | null;
  used_outputs: number;
  results: {
    model: string;
    display_name: string;
    probability: number | null;
    score: number;
  }[];
  decision?: "scored" | "partial" | "unscorable" | "not_model";
  disposition?: "downgrade" | "notDowngrade" | "unknown";
  disposition_basis?: "aggregate" | "sample_family" | null;
  disposition_group_indexes?: number[];
  verdict_mode?: VerdictMode;
  degraded_model_id?: string;
  initial?: boolean;
  ambiguous?: boolean;
  diagnostics?: {
    parsed_numbers: number;
    minimum_numbers: number;
    accepted: boolean;
  }[];
  bank_version: Record<string, unknown>;
};
export type ProviderTestGroup = {
  index: number;
  expected: number;
  status: string;
  attempts: number;
  elapsedMs: number;
  returnedModel: string | null;
  report: FingerprintReport | null;
  diagnostics: Record<string, unknown>;
  error: string | null;
  retryable: boolean;
  uncertain: boolean;
};
export type ProviderTestJob = {
  id: string;
  clientId: ClientId;
  providerId: string;
  providerName: string;
  providerVersion: string;
  endpoint: string | null;
  kind: ProviderTestKind;
  mode: ProviderTestMode;
  requestedModel: string;
  forwardedModel: string;
  returnedModel: string | null;
  status: string;
  createdAt: number;
  startedAt: number | null;
  completedAt: number | null;
  elapsedMs: number;
  firstByteMs: number | null;
  error: string | null;
  report: FingerprintReport | null;
  groups: ProviderTestGroup[];
  automatic: boolean;
  canRetry: boolean;
  completionReason: string | null;
  engine: DetectionEngine;
  sampleCount: number;
  resourceVersion: Record<string, unknown>;
  tokenizer: boolean;
  verdictMode?: VerdictMode;
  degradedModelId?: string;
  tokenizerResult: {
    status: string;
    error?: string;
    baselineDrift?: boolean;
    steps: {
      probe: string | null;
      state: string;
      tokens?: number;
      error?: string;
    }[];
    verdict?: {
      kind: string;
      top: { id: string };
      confidence: number;
      claim: { status: string; probability: number } | null;
    } | null;
  } | null;
};
export type ProviderTestError = {
  httpStatus: number | null;
  message: string;
  body: string;
  truncated: boolean;
};
export type ProviderTestDetail = {
  job: ProviderTestJob;
  output: {
    text: string;
    groups: Record<string, string>;
    diagnostics: Record<string, unknown>;
    error: ProviderTestError | null;
    groupErrors: Record<string, ProviderTestError>;
  } | null;
};
export type ProviderDetection = {
  enabled: boolean;
  mode: "smart" | "fixed";
  intervalMinutes: number;
  minIntervalMinutes: number;
  model: string;
  endpoint: string | null;
};
export type ProviderDegradation = {
  providerId: string;
  requestedModel: string;
  predictedModel: string;
  probability: number | null;
  engine: DetectionEngine;
  verdictMode?: VerdictMode;
  degradedModelId?: string;
  basis?: "aggregate" | "sample_family" | null;
  groupIndexes?: number[];
  queueRemoved: boolean;
  error: string | null;
};
export type FingerprintBankState = {
  version?: Record<string, unknown>;
  source?: string;
  models: { id: string; name: string }[];
  checkedAt?: number | null;
  updatedAt?: number | null;
  result?: "updated" | "up-to-date" | "failed" | null;
  updating: boolean;
  error: string | null;
};
export type ProviderTestingState = {
  revision: string;
  connectionPrompt: string;
  jobs: ProviderTestJob[];
  /** Model picked for the last manual test of each kind; empty when none or "default". */
  lastModels: Record<ProviderTestKind, string>;
  detection: ProviderDetection;
  degraded: Record<string, ProviderDegradation>;
  banks: Record<DetectionEngine, FingerprintBankState>;
  platform: DetectionPlatform;
  error: string | null;
  detectionStatus: {
    providerId: string;
    status: string;
    model: string;
    recentRequests: number | null;
    lastStartedAt: number | null;
    intervalMinutes: number | null;
    nextAt: number | null;
    cooldownUntil: number | null;
    error: string | null;
  }[];
};
export type ProviderTestModels = {
  models: { id: string; name: string }[];
  defaultModel: string;
  fetchedAt: number | null;
  error: string | null;
  errorDetail?: ProviderTestError | null;
};

export type PendingImport = {
  id: string;
  provider: {
    clientId: ClientId | null;
    protocol?: GrokProtocol | null;
    model?: string | null;
    name: string;
    baseUrl: string;
  } | null;
  importError: string | null;
  error: string | null;
};
export type AccountImportPreview = {
  token: string;
  revision: string;
  rows: {
    id: number;
    name: string;
    format: string;
    identityHint: string | null;
    duplicate: boolean;
    error: string | null;
  }[];
};
export type AccountExportPreview = {
  revision: string;
  rows: { id: string; name: string; error: string | null }[];
};
export type DshOptions = {
  profile: string;
  protocol: GrokProtocol;
  model: string | null;
  makeDefault: boolean;
};
export type DshPreview = {
  profiles: string[];
  profile: string | null;
  revision: string;
  models: string[];
  attached: boolean;
  protocol: GrokProtocol;
  model: string | null;
  makeDefault: boolean;
  warning: string | null;
  current: CurrentConnection | null;
  states: DshProfileCurrent[];
  providers: {
    profile: string;
    name: string;
    models: string[];
    error: string | null;
  }[];
};
export type GrokImportPreview = {
  layers?: string[];
  modelSource?: string | null;
  unknown?: string[];
  token: string | null;
  revision: string;
  models: GrokModel[];
  selected: string;
  error: string | null;
};
export type GatewayModels = {
  clientId: ClientId;
  passthrough: boolean;
  hint: string;
  models: {
    id: string;
    providers: { id: string; name: string }[];
    manual: boolean;
    /** Antigravity: the model `agy` starts on, listed first. */
    first?: boolean;
    source: string | null;
    pinned: string | null;
    metadata: ModelMetadata | null;
    custom: { context: number | null; efforts: string[] | null } | null;
    price: {
      source: string;
      inputPerM: string | null;
      outputPerM: string | null;
      cacheReadPerM: string | null;
      deviation: boolean;
    } | null;
  }[];
  providers: {
    id: string;
    name: string;
    count: number;
    checkedAt: number | null;
    stale: boolean;
    error: string | null;
  }[];
  modelsDev: ModelsDevState;
  error: string | null;
};
export type ModelSearch = {
  hits: {
    source: string;
    provider: string;
    providerName: string;
    model: string;
    name: string | null;
    context: number | null;
    output: number | null;
    cost: Record<string, unknown> | null;
  }[];
  error: string | null;
};
export type DshFileKey =
  "profilePatch" | "homePatch" | "settings" | "environment" | "credentials";
export type DshFileView = {
  profiles: string[];
  profile: string | null;
  key: DshFileKey;
  files: DshFileKey[];
  text: string;
  revision: string;
  readOnly: boolean;
  canRestore: boolean;
};
export type DshBatch = {
  state: GatewayState;
  results: {
    profile: string;
    status: "success" | "failed" | "pending";
    error: string | null;
  }[];
};
export type GatewayEdit =
  | {
      op: "saveProvider";
      takeCompaction?: boolean;
      /** "" clears the model. */
      compactionModel?: string;
      /** "" clears the effort. */
      compactionEffort?: string;
      takeAuxiliary?: boolean;
      /** "" clears the model. */
      auxiliaryModel?: string;
      /** "" clears the effort. */
      auxiliaryEffort?: string;
      /** Codex and Claude; a Grok provider's is sent with `saveGrokProvider`. */
      takeWebSearch?: boolean;
      /** "" clears the model. */
      webSearchModel?: string;
      /** "" clears the effort. */
      webSearchEffort?: string;
      endOnMessageStop?: boolean;
      filterCountTokens?: boolean;
      bufferCompleteResponse?: boolean;
      endlessMode?: boolean;
      upstreamRetrySeconds?: number;
      limitRetrySeconds?: number;
      limitRestSeconds?: number;
      normalRetrySeconds?: number;
      riskCodes?: number[];
      retryIntensity?: number;
      id?: string | null;
      baseUrl: string;
      token: string;
      quotaAccessToken?: string;
      name?: string | null;
    }
  | {
      op: "saveGrokProvider";
      bufferCompleteResponse?: boolean;
      takeWebSearch?: boolean;
      /** "" clears the model. */
      webSearchModel?: string;
      /** "" clears the effort. */
      webSearchEffort?: string;
      id?: string | null;
      baseUrl: string;
      token: string;
      quotaAccessToken?: string;
      name?: string | null;
      protocol: GrokProtocol;
    }
  | {
      op: "saveDshProvider";
      bufferCompleteResponse?: boolean;
      endlessMode?: boolean;
      upstreamRetrySeconds?: number;
      limitRetrySeconds?: number;
      limitRestSeconds?: number;
      normalRetrySeconds?: number;
      riskCodes?: number[];
      retryIntensity?: number;
      id?: string | null;
      baseUrl: string;
      token: string;
      quotaAccessToken?: string;
      name?: string | null;
      protocol: GrokProtocol;
    }
  | { op: "import" }
  | {
      op: "dshModelsProvider";
      id: string;
      mapping: Record<string, string | null>;
    }
  | { op: "modelMapProvider"; id: string; mapping: Record<string, string> }
  | { op: "renameProvider"; id: string; name: string }
  | { op: "endpointsProvider"; id: string; baseUrls: string[] }
  | { op: "swapEndpoint"; id: string; baseUrl: string }
  | {
      op: "deleteProvider" | "duplicateProvider" | "select" | "reset";
      id: string;
    }
  | { op: "queueProvider"; id: string; queued: boolean }
  | { op: "concurrencyProvider"; id: string; maxConcurrency: number }
  | { op: "rpmProvider"; id: string; maxRpm: number }
  | { op: "modelsProvider"; id: string; allowedModels: string[] | null }
  | {
      op: "policyProvider";
      id: string;
      allowedModels: string[] | null;
      handoffAfterCompaction?: boolean;
      takeNewThreads?: boolean;
    }
  | {
      op: "takeoverProvider";
      id: string;
      handoffAfterCompaction?: boolean;
      takeNewThreads?: boolean;
    }
  | { op: "reorder"; ids: string[] }
  | { op: "mode" | "connection"; mode: string }
  | { op: "modelCatalog"; enabled: boolean }
  | { op: "modelCatalogSource"; official: boolean }
  | {
      op: "upsertFailureRule";
      rule: import("./failure-policy-types").FailureRule;
    }
  | { op: "deleteFailureRule"; id: string }
  | { op: "settings"; settings: GatewaySettings };
type Call<A, R> = { args: A; result: R };
type None = Record<string, never>;
type Client = { clientId: ClientId };
type Provider = Client & { providerId: string };
type Revision = { expectedRevision: string };
export type Machine = {
  id: string;
  name: string;
  sshHost: string;
  fallbackHost: string | null;
  useSudo: boolean;
};
export type InstanceState = {
  revision: string;
  /** "local" or a machine id. */
  active: string;
  machines: Machine[];
  connection: {
    status: "local" | "connecting" | "connected" | "offline";
    version: string | null;
    error: string | null;
    /** Shared command names the remote server does not implement. */
    unsupported: string[];
  };
};
/** Lists every key of `M`; a missing or unknown name is a type error. */
const names =
  <M>() =>
  <const T extends readonly (keyof M)[]>(
    list: T &
      ([Exclude<keyof M, T[number]>] extends [never]
        ? unknown
        : { missing: Exclude<keyof M, T[number]> }),
  ): T =>
    list;
/** Commands of the desktop app itself: they always run on this machine. */
export type DesktopCommands = {
  open_main: Call<
    {
      page?: string | null;
      clientId?: ClientId;
      providerId?: string | null;
      eventId?: string | null;
      providerAction?: ProviderAction;
    },
    void
  >;
  close_main: Call<None, void>;
  ack_main_close: Call<None, void>;
  frontend_ready: Call<None, void>;
  get_startup_error: Call<None, AppError | null>;
  get_quick: Call<None, QuickPreferences>;
  set_quick: Call<
    {
      pinned?: boolean;
      hiddenClients?: ClientId[];
      selectedClient?: ClientId;
    },
    QuickPreferences
  >;
  quick_ready: Call<None, void>;
  hide_quick: Call<None, void>;
  notification_permission: Call<{ request?: boolean }, NotificationState>;
  test_notification: Call<None, NotificationState>;
  open_notification_settings: Call<None, void>;
  get_clamshell_state: Call<None, PowerState>;
  set_clamshell_awake: Call<{ enabled: boolean } & Revision, PowerState>;
  install_power_helper: Call<None, PowerState>;
  remove_power_helper: Call<None, PowerState>;
  force_quit_agent_clients: Call<
    { target: "codex" | "claude" },
    { terminated: number; failed: number; terminalCleanupFailed?: number }
  >;
  get_startup: Call<None, StartupState>;
  set_startup: Call<{ enabled: boolean }, StartupState>;
  get_provider_imports: Call<None, PendingImport[]>;
  confirm_provider_import: Call<
    {
      clientId: ClientId;
      protocol: GrokProtocol;
      id: string;
      expectedRevision: string;
    },
    void
  >;
  cancel_provider_import: Call<{ id: string }, void>;
  get_login: Call<None, LoginState>;
  start_login: Call<
    {
      mode: "browser" | "device";
      targetAccountId?: string | null;
      expectedCredentialRevision?: string | null;
    },
    LoginState
  >;
  cancel_login: Call<None, void>;
  open_login_url: Call<None, void>;
  copy_login_value: Call<{ kind: "url" | "code" }, void>;
  claude_login_status: Call<None, ClaudeLoginState>;
  start_claude_login: Call<None, ClaudeLoginState>;
  cancel_claude_login: Call<None, void>;
  /** How many accounts were written; null when the dialog was dismissed. */
  /** How many accounts were written; null when the dialog was dismissed. */
  export_antigravity_accounts: Call<
    Revision & { ids: string[] },
    number | null
  >;
  import_antigravity_accounts: Call<
    Revision,
    { state: AntigravityAuthState; added: number; skipped: number } | null
  >;
  export_usage_bill: Call<{ request: BillRequest }, BillExport | null>;
  preview_account_import: Call<None, AccountImportPreview | null>;
  commit_account_import: Call<
    Revision & { token: string; selected: { id: number; update: boolean }[] },
    ViewState
  >;
  cancel_account_exchange: Call<{ token: string }, void>;
  preview_account_export: Call<None, AccountExportPreview>;
  prepare_account_template: Call<
    { ids: string[]; replaceTemplate?: boolean },
    PreparedTemplate | null
  >;
  select_account_template: Call<{ token: string; id: number }, void>;
  export_accounts: Call<Revision & { ids: string[]; format: string }, boolean>;
  check_for_updates: Call<None, UpdateInfo>;
  open_github: Call<None, void>;
  open_update_release: Call<None, void>;
  open_pricing_directory: Call<None, void>;
  /** The provider's token, or with `quotaAccess` the one its balance is
   *  asked with. */
  reveal_provider_token: Call<
    Provider & Revision & { quotaAccess?: boolean },
    string
  >;
  get_instances: Call<None, InstanceState>;
  save_instance: Call<
    Revision & { machine: Omit<Machine, "id"> & { id: string | null } },
    InstanceState
  >;
  delete_instance: Call<Revision & { id: string }, InstanceState>;
  /** Connects and handshakes before it commits. */
  select_instance: Call<Revision & { id: string }, InstanceState>;
  test_instance: Call<
    { sshHost: string; fallbackHost: string | null; useSudo: boolean },
    { version: string }
  >;
};
/** Commands a server instance also implements: they run on the current instance. */
export type SharedCommands = {
  get_state: Call<None, ViewState>;
  switch_account: Call<{ id: string } & Revision, ViewState>;
  use_official_account: Call<
    {
      accountId: string;
      expectedAuthRevision: string;
      expectedConfigRevision: string;
    },
    ViewState
  >;
  disable_official_account: Call<None, ViewState>;
  import_current: Call<None, ViewState>;
  rename_account: Call<{ id: string; name: string }, ViewState>;
  delete_account: Call<{ id: string }, ViewState>;
  read_antigravity_config: Call<None, ConfigDocument>;
  save_antigravity_config: Call<{ text: string } & Revision, ConfigDocument>;
  read_previous_antigravity_config: Call<None, string>;
  validate_antigravity_config: Call<{ text: string }, void>;
  /** `missing` means the CLI has not created the file yet; Vela never creates it. */
  read_antigravity_cli_config: Call<
    None,
    ConfigDocument & { missing: boolean }
  >;
  save_antigravity_cli_config: Call<
    { text: string } & Revision,
    ConfigDocument & { missing: boolean }
  >;
  read_previous_antigravity_cli_config: Call<None, string>;
  validate_antigravity_cli_config: Call<{ text: string }, void>;
  read_config_tree: Call<
    { format: import("./config-tree").ConfigTreeFormat; text: string },
    import("./config-tree").ConfigTreeNode
  >;
  /** Returns the draft with one key or item replaced, added or (`value: null`) removed. */
  patch_config_tree: Call<
    {
      format: import("./config-tree").ConfigTreeFormat;
      text: string;
      path: (string | number)[];
      value: import("./config-tree").ConfigTreeValue | null;
    },
    string
  >;
  get_antigravity_auth_state: Call<None, AntigravityAuthState>;
  switch_antigravity_account: Call<
    { id: string } & Revision,
    AntigravityAuthState
  >;
  rename_antigravity_account: Call<
    { id: string; name: string } & Revision,
    AntigravityAuthState
  >;
  delete_antigravity_account: Call<
    { id: string } & Revision,
    AntigravityAuthState
  >;
  read_config: Call<Client, ConfigDocument>;
  read_previous_config: Call<Client, string>;
  validate_config: Call<Client & { text: string }, void>;
  save_config: Call<Client & { text: string } & Revision, ConfigDocument>;
  set_preferences: Call<{ preferences: Preferences }, ViewState>;
  set_quota_refresh: Call<
    { seconds: number; expectedSeconds: number },
    ViewState
  >;
  cancel_gateway_wait: Call<
    Client & { requestId: string },
    "resent" | "ended" | "gone"
  >;
  get_gateway: Call<Client, GatewayState>;
  /** What the native macOS quick panel draws of a client; no page calls it. */
  quick_snapshot: Call<Client & { cancelling?: string[] }, unknown>;
  get_gateway_freeze: Call<None, GatewayFreeze>;
  set_gateway_frozen: Call<
    { frozen: boolean; clientId?: ClientId },
    GatewayFreeze
  >;
  set_gateway_freeze_resume: Call<
    { clientId: ClientId; resumeAt: number | null },
    GatewayFreeze
  >;
  get_gateway_models: Call<Client, GatewayModels>;
  refresh_gateway_models: Call<
    Client & { providerIds?: string[] | null },
    GatewayModels
  >;
  update_codex_catalog: Call<None, GatewayState>;
  search_models_dev: Call<{ query: string; limit?: number }, ModelSearch>;
  edit_model_list: Call<
    Client & {
      op: "add" | "remove" | "pin" | "unpin" | "custom" | "first";
      id: string;
      source?: string | null;
      context?: number | null;
      efforts?: string[] | null;
    },
    GatewayModels
  >;
  dsh_official_models: Call<{ refresh: boolean }, DshOfficialCatalog>;
  read_dsh_file: Call<{ profile: string | null; key: DshFileKey }, DshFileView>;
  read_previous_dsh_file: Call<
    { profile: string | null; key: DshFileKey },
    string
  >;
  save_dsh_file: Call<
    { profile: string | null; key: DshFileKey; text: string } & Revision,
    DshFileView
  >;
  restore_dsh_file: Call<
    { profile: string | null; key: DshFileKey } & Revision,
    DshFileView
  >;
  attach_dsh_profiles: Call<
    { options: DshOptions; all: boolean; previewRevision: string } & Revision,
    DshBatch
  >;
  preview_dsh_profile: Call<{ profile?: string | null }, DshPreview>;
  preview_grok_import: Call<{ selected?: string | null }, GrokImportPreview>;
  commit_grok_import: Call<{ token: string } & Revision, GatewayState>;
  cancel_grok_import: Call<{ token?: string | null }, void>;
  probe_provider_endpoints: Call<Provider, GatewayState>;
  update_gateway: Call<
    Client &
      Revision & { edit: GatewayEdit; expectedConfigRevision?: string | null },
    GatewayState
  >;
  start_gateway: Call<
    Client & Revision & { expectedConfigRevision?: string | null },
    GatewayState
  >;
  stop_gateway: Call<
    Client & { expectedConfigRevision?: string | null },
    GatewayState
  >;
  query_provider_quota: Call<Provider & { force: boolean }, ProviderQuota>;
  list_provider_models: Call<Provider & { force: boolean }, ModelCatalog>;
  /** Upstream error statuses met by the providers sharing the host, most frequent first. */
  get_provider_seen_statuses: Call<
    { clientId: ClientId; providerId?: string; baseUrl?: string },
    {
      status: number;
      count: number;
      basis: string | null;
      lastAt: number;
    }[]
  >;
  get_app_events: Call<
    {
      filter: {
        origin?: import("./EventLog").EventOrigin | null;
        from?: number | null;
        to?: number | null;
        clientId?: ClientId | null;
        providerId?: string | null;
        level?: EventRecord["level"] | null;
        category?: EventRecord["category"] | null;
        reason?: EventRecord["reason"] | null;
        statusGroup?:
          "success" | "client_error" | "server_error" | "no_status" | null;
        page?: number;
      };
    },
    EventPage
  >;
  get_app_event: Call<{ id: string }, EventRecord | null>;
  get_app_event_raw: Call<
    { id: string },
    import("./failure-policy-types").ErrorResponse | null
  >;
  clear_app_events: Call<None, void>;
  provider_test_state: Call<
    { clientId: ClientId; providerId: string },
    ProviderTestingState
  >;
  provider_test_save_prompt: Call<{ prompt: string }, void>;
  provider_test_set_platform: Call<
    { clientId: ClientId; platform: DetectionPlatform },
    void
  >;
  provider_test_start: Call<{ request: ProviderTestRequest }, ProviderTestJob>;
  provider_test_query: Call<{ id: string }, ProviderTestDetail>;
  provider_test_cancel: Call<{ id: string }, void>;
  provider_test_retry: Call<{ id: string }, ProviderTestJob>;
  provider_test_set_detection: Call<
    {
      clientId: ClientId;
      providerId: string;
      detection: ProviderDetection;
      expectedRevision: string;
    },
    void
  >;
  provider_test_clear_mark: Call<
    { clientId: ClientId; providerId: string; expectedRevision: string },
    void
  >;
  provider_test_models: Call<
    {
      clientId: ClientId;
      providerId: string;
      expectedRevision: string;
      endpoint: string | null;
      refresh?: boolean;
    },
    ProviderTestModels
  >;
  provider_test_update_bank: Call<
    { engine: DetectionEngine },
    FingerprintBankState
  >;
  get_claude_profile: Call<None, ClaudeProfileState>;
  switch_claude_profile: Call<
    Revision & { mode: "official" | "api" },
    ClaudeProfileState
  >;
  recover_claude_profile: Call<None, ClaudeProfileState>;
  get_usage_state: Call<None, UsageState>;
  get_usage_dashboard: Call<{ filter: UsageFilter }, Dashboard>;
  get_antigravity_usage_stats: Call<
    { filter: UsageFilter },
    AntigravityUsageStats
  >;
  get_claude_usage_stats: Call<{ filter: UsageFilter }, AntigravityUsageStats>;
  get_claude_usage_quota: Call<{ refresh: boolean }, ClaudeUsageQuota>;
  get_usage_logs: Call<{ filter: UsageFilter }, UsagePage>;
  get_usage_detail: Call<{ id: string }, UsageRecord>;
  set_usage_settings: Call<{ settings: UsageSettings }, UsageState>;
  sync_usage: Call<{ rebuild?: string }, UsageState>;
  get_pricing: Call<None, PricingView>;
  calculate_usage_cost: Call<{ input: CostCalculatorInput }, CostCalculation>;
  get_usage_calculator_models: Call<{ client: string }, CalculatorModel[]>;
  /** Model IDs Claude clients asked the gateway for over the last 7 days. */
  get_claude_requested_models: Call<None, { model: string; count: number }[]>;
  get_usage_calculator_totals: Call<{ filter: UsageFilter }, Totals>;
  search_pricing: Call<
    { query: string; offset: number; limit: number },
    PricingSearchPage
  >;
  get_models_dev: Call<None, ModelsDevState>;
  update_models_dev: Call<None, ModelsDevState>;
  compare_model_prices: Call<
    { model: string; context: number },
    PriceComparison
  >;
  configure_pricing: Call<Revision & { config: PricingConfig }, PricingView>;
  update_pricing: Call<None, PricingView>;
  reload_pricing: Call<None, PricingView>;
};
export const desktopCommands = names<DesktopCommands>()([
  "open_main",
  "close_main",
  "ack_main_close",
  "frontend_ready",
  "get_startup_error",
  "get_quick",
  "set_quick",
  "quick_ready",
  "hide_quick",
  "notification_permission",
  "test_notification",
  "open_notification_settings",
  "get_clamshell_state",
  "set_clamshell_awake",
  "install_power_helper",
  "remove_power_helper",
  "force_quit_agent_clients",
  "get_startup",
  "set_startup",
  "get_provider_imports",
  "confirm_provider_import",
  "cancel_provider_import",
  "get_login",
  "start_login",
  "cancel_login",
  "open_login_url",
  "copy_login_value",
  "claude_login_status",
  "start_claude_login",
  "cancel_claude_login",
  "export_antigravity_accounts",
  "import_antigravity_accounts",
  "export_usage_bill",
  "preview_account_import",
  "commit_account_import",
  "cancel_account_exchange",
  "preview_account_export",
  "prepare_account_template",
  "select_account_template",
  "export_accounts",
  "check_for_updates",
  "open_github",
  "open_update_release",
  "open_pricing_directory",
  "reveal_provider_token",
  "get_instances",
  "save_instance",
  "delete_instance",
  "select_instance",
  "test_instance",
]);
export const sharedCommands = names<SharedCommands>()([
  "get_state",
  "switch_account",
  "use_official_account",
  "disable_official_account",
  "import_current",
  "rename_account",
  "delete_account",
  "read_antigravity_config",
  "save_antigravity_config",
  "read_previous_antigravity_config",
  "validate_antigravity_config",
  "read_antigravity_cli_config",
  "save_antigravity_cli_config",
  "read_previous_antigravity_cli_config",
  "validate_antigravity_cli_config",
  "read_config_tree",
  "patch_config_tree",
  "get_antigravity_auth_state",
  "switch_antigravity_account",
  "rename_antigravity_account",
  "delete_antigravity_account",
  "read_config",
  "read_previous_config",
  "validate_config",
  "save_config",
  "set_preferences",
  "set_quota_refresh",
  "cancel_gateway_wait",
  "get_gateway",
  "quick_snapshot",
  "get_gateway_freeze",
  "set_gateway_frozen",
  "set_gateway_freeze_resume",
  "get_gateway_models",
  "refresh_gateway_models",
  "update_codex_catalog",
  "search_models_dev",
  "edit_model_list",
  "dsh_official_models",
  "read_dsh_file",
  "read_previous_dsh_file",
  "save_dsh_file",
  "restore_dsh_file",
  "attach_dsh_profiles",
  "preview_dsh_profile",
  "preview_grok_import",
  "commit_grok_import",
  "cancel_grok_import",
  "probe_provider_endpoints",
  "update_gateway",
  "start_gateway",
  "stop_gateway",
  "query_provider_quota",
  "list_provider_models",
  "get_provider_seen_statuses",
  "get_app_events",
  "get_app_event",
  "get_app_event_raw",
  "clear_app_events",
  "provider_test_state",
  "provider_test_save_prompt",
  "provider_test_set_platform",
  "provider_test_start",
  "provider_test_query",
  "provider_test_cancel",
  "provider_test_retry",
  "provider_test_set_detection",
  "provider_test_clear_mark",
  "provider_test_models",
  "provider_test_update_bank",
  "get_claude_profile",
  "switch_claude_profile",
  "recover_claude_profile",
  "get_usage_state",
  "get_usage_dashboard",
  "get_antigravity_usage_stats",
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
  "get_models_dev",
  "update_models_dev",
  "compare_model_prices",
  "configure_pricing",
  "update_pricing",
  "reload_pricing",
]);
export type CommandMap = DesktopCommands & SharedCommands;
export type CommandName = keyof CommandMap;
export type CommandArgs<K extends CommandName> = CommandMap[K]["args"];
export type CommandResult<K extends CommandName> = CommandMap[K]["result"];
export type CommandParams<K extends CommandName> =
  {} extends CommandArgs<K> ? [args?: CommandArgs<K>] : [args: CommandArgs<K>];
export type FreezeStatus = {
  frozen: boolean;
  draining: number;
  receiving: number;
  waiting: number;
  headers: number;
  streaming: number;
  finishing: number;
  queued: number;
};
export type ClientFreeze = FreezeStatus & {
  clientId: ClientId;
  resumeAt: number | null;
};
export type GatewayFreeze = FreezeStatus & {
  partial: boolean;
  clients: ClientFreeze[];
};
/** Events a server instance also emits: delivered from the current instance. */
export type SharedEvents = {
  "switch-state": ViewState;
  "switch-error": AppError;
  "switch-notice": string;
  "gateway-state": GatewayState;
  "gateway-freeze": GatewayFreeze;
  "provider-quota": { clientId: ClientId; quota: ProviderQuota };
  "config-state": {
    clientId: ConfigClientId;
    revision: string;
    guarded: boolean;
  };
  "claude-profile-state": ClaudeProfileState;
  "antigravity-auth-state": AntigravityAuthState;
  "provider-tests": null;
  "usage-state": null;
  "pricing-state": null;
  /** The journal changed; the log page reloads rather than read the payload. */
  "app-event": unknown;
};
/** Events of the desktop app itself. */
export type DesktopEvents = {
  "instance-state": InstanceState;
  "login-state": LoginState;
  "clamshell-state": PowerState;
  "clamshell-error": AppError | null;
  "notification-state": NotificationState;
  "claude-login-state": ClaudeLoginState;
  "quick-state": QuickPreferences;
  "app-visibility": boolean;
  "panel-visibility": boolean;
  "provider-imports": null;
  "event-log": { id: string; clientId: ClientId; providerId: string | null };
  "provider-settings": {
    id: string;
    clientId: ClientId;
    action?: ProviderAction;
  };
  "main-close-requested": { hidden?: boolean } | null;
  navigate: string;
};
export const sharedEvents = names<SharedEvents>()([
  "switch-state",
  "switch-error",
  "switch-notice",
  "gateway-state",
  "gateway-freeze",
  "provider-quota",
  "config-state",
  "claude-profile-state",
  "antigravity-auth-state",
  "provider-tests",
  "usage-state",
  "pricing-state",
  "app-event",
]);
export const desktopEvents = names<DesktopEvents>()([
  "instance-state",
  "login-state",
  "clamshell-state",
  "clamshell-error",
  "notification-state",
  "claude-login-state",
  "quick-state",
  "app-visibility",
  "panel-visibility",
  "provider-imports",
  "event-log",
  "provider-settings",
  "main-close-requested",
  "navigate",
]);
export type EventMap = SharedEvents & DesktopEvents;
/** How the desktop app forwards a shared event of a remote instance. */
export type RemoteEvent = { instance: string; event: string; payload: unknown };
