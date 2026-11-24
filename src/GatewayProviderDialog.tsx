import { useCallback, useEffect, useId, useRef, useState } from "react";
import { Eye, EyeOff, LoaderCircle } from "lucide-react";
import { subscriptions } from "./subscriptions";
import { command } from "./bridge";
import { useInstance } from "./instance";
import {
  DEFAULT_LIMIT_REST_SECONDS,
  DEFAULT_LIMIT_RETRY_SECONDS,
  DEFAULT_NORMAL_RETRY_SECONDS,
  DEFAULT_RETRY_INTENSITY,
  DEFAULT_RISK_CODES,
  DEFAULT_UPSTREAM_RETRY_SECONDS,
  EFFORT_ORDER,
  errorOf,
  type ClientId,
  type GatewayState,
  type Provider,
} from "./types";
import type { GatewayEdit } from "./bridge-contracts";
import type { EditRevision } from "./gateway-edit";
import Modal from "./Modal";
import { confirmAction, confirmSave } from "./confirmation";
import ProviderSettings from "./ProviderSettings";
import ClaudeModelMapping from "./ClaudeModelMapping";
import ProviderEndpoints from "./ProviderEndpoints";
import { limitLabel } from "./limit-labels";

/** Status codes split on spaces or commas; null unless all are 400–599 and at most 16. */
const parseRiskCodes = (text: string) => {
  const parts = text.split(/[\s,，]+/).filter(Boolean);
  if (parts.some((part) => !/^[45]\d\d$/.test(part))) return null;
  const codes = [...new Set(parts.map(Number))].sort((a, b) => a - b);
  return codes.length > 16 ? null : codes;
};

const VERTEX_REGIONS = [
  "global",
  "us-east5",
  "us-central1",
  "us-east1",
  "us-west1",
  "europe-west1",
  "europe-west4",
  "asia-southeast1",
  "asia-northeast1",
];
const VERTEX_HOST = "aiplatform.googleapis.com";

/** The Vertex AI address of a publisher; the express form when no project is given. */
const vertexUrl = (publisher: string, region: string, project: string) => {
  region = region.trim();
  project = project.trim();
  if (!project) return `https://${VERTEX_HOST}/v1/publishers/${publisher}`;
  if (!region) return "";
  const host = region === "global" ? VERTEX_HOST : `${region}-${VERTEX_HOST}`;
  return `https://${host}/v1/projects/${project}/locations/${region}/publishers/${publisher}`;
};

/** The region and project of an address `vertexUrl` builds, or null for any other. */
const parseVertexUrl = (publisher: string, url: string) => {
  const address = url.trim().replace(/\/+$/, "");
  if (address === vertexUrl(publisher, "global", ""))
    return { region: "global", project: "" };
  const match =
    /^https:\/\/[^/]+\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/publishers\/[^/]+$/.exec(
      address,
    );
  if (!match) return null;
  const [, project, region] = match;
  return address === vertexUrl(publisher, region, project)
    ? { region, project }
    : null;
};

export type ProviderTab = "api" | "whitelist" | "modelMap" | "endpoints";
type ProviderDialog = { kind: "provider"; item?: Provider; tab?: ProviderTab };

function ProviderApiForm({
  clientId,
  connectionMode,
  dialog,
  revision,
  close,
  cancel,
  save,
  onDirtyChange,
  onBusyChange,
}: {
  clientId: ClientId;
  connectionMode: "bearer" | "apiKey";
  dialog: ProviderDialog;
  /** Closes the dialog once the provider is saved. */
  close: () => void;
  cancel: () => void;
  revision: string;
  save: (p: GatewayEdit, revision: EditRevision) => Promise<void>;
  onDirtyChange?: (dirty: boolean) => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const baseline = useRef<EditRevision>(revision);
  const { remote } = useInstance();
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  useEffect(() => {
    onBusyChange(busy);
  }, [busy, onBusyChange]);
  // The name the gateway holds, which a retry must not rename to again.
  const savedName = useRef(dialog.item?.name);
  const [name, setName] = useState(dialog.item ? dialog.item.name : ""),
    [baseUrl, setBaseUrl] = useState(dialog.item?.baseUrl ?? ""),
    [token, setToken] = useState("");
  // Claude and Antigravity providers can be reached through Vertex AI, whose
  // address is built from a region and a project instead of typed.
  const publisher =
    clientId === "claude" ? "anthropic" : clientId === "agy" ? "google" : null;
  const savedVertex = publisher
    ? parseVertexUrl(publisher, dialog.item?.baseUrl ?? "")
    : null;
  const [vertex, setVertex] = useState(Boolean(savedVertex)),
    [region, setRegion] = useState(savedVertex?.region ?? "global"),
    [project, setProject] = useState(savedVertex?.project ?? ""),
    [customRegion, setCustomRegion] = useState(
      Boolean(savedVertex && !VERTEX_REGIONS.includes(savedVertex.region)),
    );
  const pickVertex = (nextRegion: string, nextProject: string) => {
    setRegion(nextRegion);
    setProject(nextProject);
    if (publisher) setBaseUrl(vertexUrl(publisher, nextRegion, nextProject));
  };
  const [protocol, setProtocol] = useState(
    clientId === "dsh"
      ? "chat_completions"
      : (dialog.item?.protocol ?? "responses"),
  );
  const [showToken, setShowToken] = useState(false);
  const [quotaAccessToken, setQuotaAccessToken] = useState<
    string | undefined
  >();
  const [endOnMessageStop, setEndOnMessageStop] = useState(
    Boolean(dialog.item?.endOnMessageStop),
  );
  const [filterCountTokens, setFilterCountTokens] = useState(
    Boolean(dialog.item?.filterCountTokens),
  );
  const [bufferCompleteResponse, setBufferCompleteResponse] = useState(
    Boolean(dialog.item?.bufferCompleteResponse),
  );
  const [endlessMode, setEndlessMode] = useState(
    Boolean(dialog.item?.endlessMode),
  );
  // What the endless block sends while it is switched off.
  const savedEndless = {
    upstreamRetrySeconds:
      dialog.item?.upstreamRetrySeconds ?? DEFAULT_UPSTREAM_RETRY_SECONDS,
    limitRetrySeconds:
      dialog.item?.limitRetrySeconds ?? DEFAULT_LIMIT_RETRY_SECONDS,
    limitRestSeconds:
      dialog.item?.limitRestSeconds ?? DEFAULT_LIMIT_REST_SECONDS,
    normalRetrySeconds:
      dialog.item?.normalRetrySeconds ?? DEFAULT_NORMAL_RETRY_SECONDS,
    riskCodes: dialog.item?.riskCodes ?? DEFAULT_RISK_CODES[clientId],
    retryIntensity: dialog.item?.retryIntensity ?? DEFAULT_RETRY_INTENSITY,
  };
  const [upstreamRetrySeconds, setUpstreamRetrySeconds] = useState(
    String(savedEndless.upstreamRetrySeconds),
  );
  const [limitRetrySeconds, setLimitRetrySeconds] = useState(
    String(savedEndless.limitRetrySeconds),
  );
  const [limitRestSeconds, setLimitRestSeconds] = useState(
    String(savedEndless.limitRestSeconds),
  );
  const [normalRetrySeconds, setNormalRetrySeconds] = useState(
    String(savedEndless.normalRetrySeconds),
  );
  // The typed risk codes; the ticked ones live in `checkedCodes`.
  const [riskCodes, setRiskCodes] = useState(savedEndless.riskCodes.join(", "));
  const [checkedCodes, setCheckedCodes] = useState<number[]>([]);
  const riskText = useRef(riskCodes);
  riskText.current = riskCodes;
  // The error statuses to tick from: those the providers on this host have
  // answered with, then the listed codes none of them has.
  const [riskOptions, setRiskOptions] = useState<
    { status: number; count: number; basis: string | null }[]
  >([]);
  const allRiskCodes = `${checkedCodes.join(" ")} ${riskCodes}`.trim();
  // One spelling for the same list, however it is split between the two.
  const riskDraft = parseRiskCodes(allRiskCodes)?.join(",") ?? allRiskCodes;
  const [retryIntensity, setRetryIntensity] = useState(
    String(savedEndless.retryIntensity),
  );
  // Codex and Claude providers can serve the compaction and auxiliary requests
  // in place of the rest of the queue. A new provider takes every kind its
  // client hands over.
  const takeoverOffered = clientId === "claude" || clientId === "codex";
  const [takeCompaction, setTakeCompaction] = useState(
    dialog.item ? Boolean(dialog.item.takeCompaction) : true,
  );
  const [takeAuxiliary, setTakeAuxiliary] = useState(
    dialog.item ? Boolean(dialog.item.takeAuxiliary) : true,
  );
  // Codex, Claude and Grok providers can serve the web searches their client
  // sends. Grok's is a Responses request, which a provider on another
  // protocol is never sent.
  const searchOffered =
    clientId === "claude" ||
    clientId === "codex" ||
    (clientId === "grok" && protocol === "responses");
  const [takeWebSearch, setTakeWebSearch] = useState(
    dialog.item ? Boolean(dialog.item.takeWebSearch) : true,
  );
  // What a takeover's model and effort send while its option is switched off
  // or hidden.
  const savedTakeover = {
    compactionModel: dialog.item?.compactionModel ?? "",
    compactionEffort: dialog.item?.compactionEffort ?? "",
    auxiliaryModel: dialog.item?.auxiliaryModel ?? "",
    auxiliaryEffort: dialog.item?.auxiliaryEffort ?? "",
    webSearchModel: dialog.item?.webSearchModel ?? "",
    webSearchEffort: dialog.item?.webSearchEffort ?? "",
  };
  const [compactionModel, setCompactionModel] = useState(
    savedTakeover.compactionModel,
  );
  const [compactionEffort, setCompactionEffort] = useState(
    savedTakeover.compactionEffort,
  );
  const [auxiliaryModel, setAuxiliaryModel] = useState(
    savedTakeover.auxiliaryModel,
  );
  const [auxiliaryEffort, setAuxiliaryEffort] = useState(
    savedTakeover.auxiliaryEffort,
  );
  const [webSearchModel, setWebSearchModel] = useState(
    savedTakeover.webSearchModel,
  );
  const [webSearchEffort, setWebSearchEffort] = useState(
    savedTakeover.webSearchEffort,
  );
  // The efforts a takeover may name, weakest first as the client's API spells
  // them, with a saved one that is not among them.
  const efforts = (saved: string) => {
    const known =
      clientId === "claude"
        ? ["low", "medium", "high", "xhigh", "max"]
        : EFFORT_ORDER;
    return saved && !known.includes(saved) ? [...known, saved] : known;
  };
  // The provider's listed models, offered as suggestions for a takeover's model.
  const [models, setModels] = useState<string[]>([]);
  const suggestions = useId();
  const providerId = dialog.item?.id;
  const suggesting =
    (takeoverOffered && (takeCompaction || takeAuxiliary)) ||
    (searchOffered && takeWebSearch);
  useEffect(() => {
    if (!providerId || !suggesting) return;
    let gone = false;
    void (async () => {
      // The saved list shows at once; the provider's current one replaces it.
      for (const force of [false, true]) {
        try {
          const catalog = await command("list_provider_models", {
            clientId,
            providerId,
            force,
          });
          if (gone) return;
          setModels(catalog.models);
        } catch {
          // Suggestions only: the model stays typeable without them.
        }
      }
    })();
    return () => {
      gone = true;
    };
  }, [clientId, providerId, suggesting]);
  const seenAddress = providerId ? undefined : baseUrl;
  useEffect(() => {
    if (!endlessMode || clientId === "grok") return;
    if (!providerId && !seenAddress) return;
    let gone = false;
    void (async () => {
      try {
        const seen = await command(
          "get_provider_seen_statuses",
          providerId
            ? { clientId, providerId }
            : { clientId, baseUrl: seenAddress },
        );
        if (gone || !seen?.length) return;
        // Typed codes that are error statuses move to the list, ticked.
        const parts = riskText.current.split(/[\s,，]+/).filter(Boolean);
        const moved = [
          ...new Set(
            parts.filter((part) => /^[45]\d\d$/.test(part)).map(Number),
          ),
        ];
        setRiskOptions((old) => {
          const known = [
            ...seen,
            ...old.filter((o) => !seen.some((s) => s.status === o.status)),
          ];
          return [
            ...known,
            ...moved
              .filter((status) => !known.some((o) => o.status === status))
              .map((status) => ({ status, count: 0, basis: null })),
          ];
        });
        setCheckedCodes((old) => [...new Set([...old, ...moved])]);
        setRiskCodes(
          parts.filter((part) => !/^[45]\d\d$/.test(part)).join(" "),
        );
      } catch {
        // The codes stay typeable without the list.
      }
    })();
    return () => {
      gone = true;
    };
  }, [clientId, providerId, seenAddress, endlessMode]);

  const [revealed, setRevealed] = useState<{
    value: string;
    revision: string;
  } | null>(null);
  const [revealing, setRevealing] = useState(false);
  const revealSequence = useRef(0);
  const currentRevision = useRef(revision);
  currentRevision.current = revision;
  const hideToken = useCallback(() => {
    revealSequence.current += 1;
    setShowToken(false);
    setRevealed(null);
    setRevealing(false);
  }, []);
  useEffect(() => {
    hideToken();
    const events = subscriptions((e) => setError(errorOf(e).message));
    const visibility = () => {
      if (document.visibilityState === "hidden") hideToken();
    };
    document.addEventListener("visibilitychange", visibility);
    events.watch("app-visibility", (visible) => {
      if (!visible) hideToken();
    });
    return () => {
      revealSequence.current += 1;
      events.dispose();
      document.removeEventListener("visibilitychange", visibility);
    };
  }, [clientId, dialog.item?.id, revision, hideToken]);
  const toggleToken = async () => {
    if (showToken) {
      hideToken();
      return;
    }
    setError("");
    setShowToken(true);
    if (token || !dialog.item) return;
    const sequence = ++revealSequence.current;
    setRevealing(true);
    try {
      const value = await command("reveal_provider_token", {
        clientId,
        providerId: dialog.item.id,
        expectedRevision: revision,
      });
      if (
        sequence === revealSequence.current &&
        currentRevision.current === revision
      ) {
        setRevealed({ value, revision });
      }
    } catch (e) {
      if (
        sequence === revealSequence.current &&
        currentRevision.current === revision
      ) {
        setShowToken(false);
        setError(errorOf(e).message);
      }
    } finally {
      if (sequence === revealSequence.current) setRevealing(false);
    }
  };
  // The fields that make the form dirty, as one comparable string.
  const snapshot = JSON.stringify([
    name,
    baseUrl,
    token,
    protocol,
    quotaAccessToken,
    endOnMessageStop,
    filterCountTokens,
    bufferCompleteResponse,
    endlessMode,
    upstreamRetrySeconds,
    limitRetrySeconds,
    limitRestSeconds,
    normalRetrySeconds,
    riskDraft,
    retryIntensity,
    takeCompaction,
    compactionModel,
    compactionEffort,
    takeAuxiliary,
    auxiliaryModel,
    auxiliaryEffort,
    takeWebSearch,
    webSearchModel,
    webSearchEffort,
  ]);
  const initialDraft = useRef(snapshot);
  useEffect(() => {
    onDirtyChange?.(snapshot !== initialDraft.current);
  }, [snapshot, onDirtyChange]);
  useEffect(() => () => onDirtyChange?.(false), [onDirtyChange]);
  return (
    <form
      className="gateway-form provider-form"
      onSubmit={(e) => {
        e.preventDefault();
        const codes = endlessMode
          ? parseRiskCodes(allRiskCodes)
          : savedEndless.riskCodes;
        if (!codes) {
          setError("风控代码应为 400–599 的状态码，最多 16 个");
          return;
        }
        const item = dialog.item;
        const renamed = name.trim();
        if (item && (!renamed || /[\u0000-\u001f\u007f]/.test(renamed))) {
          setError("名称应为 1–120 个可见字符");
          return;
        }
        setError("");
        setBusy(true);
        const draft = {
          id: dialog.item?.id ?? null,
          baseUrl,
          token,
          ...(quotaAccessToken !== undefined ? { quotaAccessToken } : {}),
          ...(!dialog.item ? { name } : {}),
        };
        const endless = endlessMode
          ? {
              endlessMode,
              upstreamRetrySeconds: Number(upstreamRetrySeconds),
              limitRetrySeconds: Number(limitRetrySeconds),
              limitRestSeconds: Number(limitRestSeconds),
              normalRetrySeconds: Number(normalRetrySeconds),
              riskCodes: codes,
              retryIntensity: Number(retryIntensity),
            }
          : { endlessMode, ...savedEndless };
        const compacting = takeoverOffered && takeCompaction,
          assisting = takeoverOffered && takeAuxiliary;
        const takeover = {
          takeCompaction: compacting,
          compactionModel: (compacting
            ? compactionModel
            : savedTakeover.compactionModel
          ).trim(),
          compactionEffort: compacting
            ? compactionEffort
            : savedTakeover.compactionEffort,
          takeAuxiliary: assisting,
          auxiliaryModel: (assisting
            ? auxiliaryModel
            : savedTakeover.auxiliaryModel
          ).trim(),
          auxiliaryEffort: assisting
            ? auxiliaryEffort
            : savedTakeover.auxiliaryEffort,
        };
        const searching = searchOffered && takeWebSearch;
        const search = {
          takeWebSearch: searching,
          webSearchModel: (searching
            ? webSearchModel
            : savedTakeover.webSearchModel
          ).trim(),
          webSearchEffort: searching
            ? webSearchEffort
            : savedTakeover.webSearchEffort,
        };
        const payload: GatewayEdit =
          clientId === "grok"
            ? { op: "saveGrokProvider", ...draft, protocol, ...search }
            : clientId === "dsh"
              ? { op: "saveDshProvider", ...draft, protocol, ...endless }
              : {
                  op: "saveProvider",
                  ...draft,
                  ...endless,
                  ...takeover,
                  ...(clientId === "codex" ? search : {}),
                  ...(clientId === "claude"
                    ? {
                        ...search,
                        endOnMessageStop,
                        filterCountTokens,
                        bufferCompleteResponse,
                      }
                    : {}),
                };
        void (async () => {
          let expected = baseline.current;
          if (item && renamed !== savedName.current) {
            await save(
              { op: "renameProvider", id: item.id, name: renamed },
              expected,
            );
            savedName.current = renamed;
            // The rename advanced the revision: write against the new one.
            expected = null;
          }
          await save(payload, expected);
          close();
        })()
          .catch((e) => {
            baseline.current = null;
            setError(errorOf(e).message);
          })
          .finally(() => setBusy(false));
      }}
    >
      <>
        <label>
          名称
          <input
            type="text"
            maxLength={120}
            required={!!dialog.item}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={dialog.item ? undefined : "可选"}
          />
        </label>
        {clientId === "grok" && (
          <label>
            API 协议
            <select
              value={protocol}
              onChange={(e) => setProtocol(e.target.value as typeof protocol)}
            >
              <option value="responses">Responses</option>
              <option value="chat_completions">Chat Completions</option>
              <option value="messages">Messages</option>
            </select>
          </label>
        )}
        {publisher && (
          <div className="segmented" role="group" aria-label="接入方式">
            <button
              type="button"
              aria-pressed={!vertex}
              disabled={busy}
              onClick={() => setVertex(false)}
            >
              普通端点
            </button>
            <button
              type="button"
              aria-pressed={vertex}
              disabled={busy}
              onClick={() => {
                setVertex(true);
                pickVertex(region, project);
              }}
            >
              Vertex AI
            </button>
          </div>
        )}
        {vertex && (
          <>
            <label>
              区域
              <select
                value={customRegion ? "" : region}
                disabled={busy}
                onChange={(e) => {
                  setCustomRegion(!e.target.value);
                  pickVertex(e.target.value, project);
                }}
              >
                {VERTEX_REGIONS.map((item) => (
                  <option key={item} value={item}>
                    {item}
                  </option>
                ))}
                <option value="">自定义…</option>
              </select>
            </label>
            {customRegion && (
              <label>
                自定义区域
                <input
                  type="text"
                  required
                  spellCheck={false}
                  value={region}
                  disabled={busy}
                  placeholder="us-east5"
                  onChange={(e) => pickVertex(e.target.value, project)}
                />
              </label>
            )}
            <label>
              项目 ID
              <input
                type="text"
                spellCheck={false}
                value={project}
                disabled={busy}
                placeholder="可选；留空使用不分区域的 express 端点"
                onChange={(e) => pickVertex(region, e.target.value)}
              />
            </label>
          </>
        )}
        <label>
          <span className="field-label-row">
            <span>
              {clientId === "claude" ? "ANTHROPIC_BASE_URL" : "base_url"}
            </span>
            {clientId === "dsh" && !dialog.item && (
              <button
                type="button"
                className="text-button"
                onClick={() => {
                  if (!name.trim()) setName("DeepSeek Official");
                  setBaseUrl("https://api.deepseek.com/v1");
                }}
              >
                填入 DeepSeek 官方地址
              </button>
            )}
          </span>
          <input
            required
            type="url"
            autoFocus
            readOnly={vertex}
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder={
              clientId === "claude"
                ? "https://api.example.invalid"
                : clientId === "agy"
                  ? "https://generativelanguage.googleapis.com"
                  : clientId === "dsh"
                    ? "https://api.deepseek.com/v1"
                    : "https://api.example.com/v1"
            }
          />
        </label>
        <label>
          {vertex
            ? "Google API Key"
            : clientId === "claude"
              ? "ANTHROPIC_AUTH_TOKEN"
              : clientId === "grok" || clientId === "dsh"
                ? "api_key"
                : clientId === "agy"
                  ? "API Key"
                  : connectionMode === "apiKey"
                    ? "OPENAI_API_KEY"
                    : "experimental_bearer_token"}
          <span className="provider-token-input">
            <input
              required={!dialog.item}
              type={showToken ? "text" : "password"}
              autoComplete="new-password"
              spellCheck={false}
              value={
                showToken && revealed?.revision === revision
                  ? revealed.value
                  : token
              }
              onChange={(e) => {
                revealSequence.current += 1;
                setRevealed(null);
                setRevealing(false);
                setToken(e.target.value);
              }}
              placeholder={dialog.item ? "留空保留当前 Token" : "输入 Token"}
            />
            <button
              type="button"
              className="icon-button"
              aria-label={showToken ? "隐藏 Token" : "显示 Token"}
              aria-pressed={showToken}
              aria-busy={revealing}
              // A saved token is revealed from this machine's store only.
              disabled={remote && !token && !!dialog.item}
              title={
                remote && !token && dialog.item
                  ? "服务器上保存的 Token 不能在本机显示"
                  : undefined
              }
              onClick={() => void toggleToken()}
            >
              {revealing ? (
                <LoaderCircle size={16} className="spin" aria-hidden="true" />
              ) : showToken ? (
                <EyeOff size={16} aria-hidden="true" />
              ) : (
                <Eye size={16} aria-hidden="true" />
              )}
            </button>
          </span>
        </label>
      </>
      <label>
        账号访问令牌（可选）
        <span className="provider-token-input">
          <input
            type="password"
            autoComplete="new-password"
            spellCheck={false}
            value={quotaAccessToken ?? ""}
            onChange={(e) => setQuotaAccessToken(e.target.value || undefined)}
            placeholder={
              quotaAccessToken === ""
                ? "保存后清除"
                : dialog.item?.hasQuotaAccessToken
                  ? "已配置，留空保留"
                  : "用于 New API 账户余额查询"
            }
          />
          {(dialog.item?.hasQuotaAccessToken || quotaAccessToken) && (
            <button
              type="button"
              className="text-button"
              onClick={() => setQuotaAccessToken("")}
            >
              清除
            </button>
          )}
        </span>
      </label>
      {(clientId !== "grok" || searchOffered) && (
        <div className="provider-switches">
          {/* Ahead of the options, so that an opened block is the last child
                when nothing follows it. */}
          {models.length > 0 && (
            <datalist id={suggestions}>
              {models.map((model) => (
                <option key={model} value={model} />
              ))}
            </datalist>
          )}
          {clientId === "claude" && (
            <>
              <label className="provider-search-setting">
                <input
                  type="checkbox"
                  checked={endOnMessageStop}
                  disabled={busy}
                  onChange={(e) => setEndOnMessageStop(e.target.checked)}
                />
                <span>收到 message_stop 后立即结束响应</span>
              </label>
              <label className="provider-search-setting">
                <input
                  type="checkbox"
                  checked={filterCountTokens}
                  disabled={busy}
                  onChange={(e) => setFilterCountTokens(e.target.checked)}
                />
                <span>过滤 Token 计数请求</span>
              </label>
              <label className="provider-search-setting">
                <input
                  type="checkbox"
                  checked={bufferCompleteResponse}
                  disabled={busy}
                  onChange={(e) => setBufferCompleteResponse(e.target.checked)}
                />
                <span>完整响应后发送</span>
              </label>
            </>
          )}
          {takeoverOffered && (
            <>
              <label
                className="provider-search-setting"
                title="开启后，本供应商加入压缩队列：压缩请求按列表顺序交给队列里的供应商，都未能完成时返回错误，不回到普通队列"
              >
                <input
                  type="checkbox"
                  checked={takeCompaction}
                  disabled={busy}
                  onChange={(e) => setTakeCompaction(e.target.checked)}
                />
                <span>接管压缩请求</span>
              </label>
              {takeCompaction && (
                <div className="provider-retry-settings">
                  <label
                    className="provider-retry-setting provider-takeover-model"
                    title="填写后，不论客户端请求什么模型，压缩请求都改用该模型；留空则沿用客户端请求的模型"
                  >
                    <span>压缩模型</span>
                    <input
                      type="text"
                      list={models.length ? suggestions : undefined}
                      maxLength={256}
                      spellCheck={false}
                      value={compactionModel}
                      disabled={busy}
                      placeholder="留空则沿用客户端模型"
                      onChange={(e) => setCompactionModel(e.target.value)}
                    />
                  </label>
                  <label
                    className="provider-retry-setting provider-takeover-model"
                    title="选定后，压缩请求改用该思考强度，不沿用原线程的；跟随客户端则不改动"
                  >
                    <span>思考强度</span>
                    <select
                      aria-label="压缩思考强度"
                      value={compactionEffort}
                      disabled={busy}
                      onChange={(e) => setCompactionEffort(e.target.value)}
                    >
                      <option value="">跟随客户端</option>
                      {efforts(savedTakeover.compactionEffort).map((effort) => (
                        <option key={effort} value={effort}>
                          {effort}
                        </option>
                      ))}
                    </select>
                  </label>
                </div>
              )}
              <label
                className="provider-search-setting"
                title="开启后，本供应商加入辅助队列：自动审批、标题、摘要、记忆等非主对话请求按列表顺序交给队列里的供应商，都未能完成时返回错误，不回到普通队列"
              >
                <input
                  type="checkbox"
                  checked={takeAuxiliary}
                  disabled={busy}
                  onChange={(e) => setTakeAuxiliary(e.target.checked)}
                />
                <span>接管辅助请求</span>
              </label>
              {takeAuxiliary && (
                <div className="provider-retry-settings">
                  <label
                    className="provider-retry-setting provider-takeover-model"
                    title="填写后，不论客户端请求什么模型，辅助请求都改用该模型；留空则沿用客户端请求的模型"
                  >
                    <span>辅助模型</span>
                    <input
                      type="text"
                      list={models.length ? suggestions : undefined}
                      maxLength={256}
                      spellCheck={false}
                      value={auxiliaryModel}
                      disabled={busy}
                      placeholder="留空则沿用客户端模型"
                      onChange={(e) => setAuxiliaryModel(e.target.value)}
                    />
                  </label>
                  <label
                    className="provider-retry-setting provider-takeover-model"
                    title="选定后，辅助请求改用该思考强度，不沿用原线程的；跟随客户端则不改动"
                  >
                    <span>思考强度</span>
                    <select
                      aria-label="辅助思考强度"
                      value={auxiliaryEffort}
                      disabled={busy}
                      onChange={(e) => setAuxiliaryEffort(e.target.value)}
                    >
                      <option value="">跟随客户端</option>
                      {efforts(savedTakeover.auxiliaryEffort).map((effort) => (
                        <option key={effort} value={effort}>
                          {effort}
                        </option>
                      ))}
                    </select>
                  </label>
                </div>
              )}
            </>
          )}
          {searchOffered && (
            <>
              <label
                className="provider-search-setting"
                title="开启后，本供应商加入搜索队列：客户端搜索工具发出的网络搜索请求按列表顺序交给队列里的供应商，都未能完成时返回错误，不回到普通队列。请只在供应商支持网络搜索时开启"
              >
                <input
                  type="checkbox"
                  checked={takeWebSearch}
                  disabled={busy}
                  onChange={(e) => setTakeWebSearch(e.target.checked)}
                />
                <span
                  title={
                    clientId === "grok"
                      ? "Grok 的搜索模型需指向 vela 或 vela/<模型 ID>（config.toml 的 [models] web_search），搜索请求才会经过网关"
                      : undefined
                  }
                >
                  接管网络搜索请求
                </span>
              </label>
              {takeWebSearch && (
                <div className="provider-retry-settings">
                  <label
                    className="provider-retry-setting provider-takeover-model"
                    title="填写后，不论客户端请求什么模型，网络搜索请求都改用该模型；留空则沿用客户端请求的模型"
                  >
                    <span>搜索模型</span>
                    <input
                      type="text"
                      list={models.length ? suggestions : undefined}
                      maxLength={256}
                      spellCheck={false}
                      value={webSearchModel}
                      disabled={busy}
                      placeholder="留空则沿用客户端模型"
                      onChange={(e) => setWebSearchModel(e.target.value)}
                    />
                  </label>
                  <label
                    className="provider-retry-setting provider-takeover-model"
                    title="选定后，网络搜索请求改用该思考强度；跟随客户端则不改动"
                  >
                    <span>思考强度</span>
                    <select
                      aria-label="搜索思考强度"
                      value={webSearchEffort}
                      disabled={busy}
                      onChange={(e) => setWebSearchEffort(e.target.value)}
                    >
                      <option value="">跟随客户端</option>
                      {efforts(savedTakeover.webSearchEffort).map((effort) => (
                        <option key={effort} value={effort}>
                          {effort}
                        </option>
                      ))}
                    </select>
                  </label>
                </div>
              )}
            </>
          )}
          {clientId !== "grok" && (
            <label className="provider-search-setting">
              <input
                type="checkbox"
                checked={endlessMode}
                disabled={busy}
                onChange={(e) => setEndlessMode(e.target.checked)}
              />
              <span>无尽模式</span>
            </label>
          )}
          {clientId !== "grok" && endlessMode && (
            <div className="provider-retry-settings">
              <label
                className="provider-retry-setting"
                title="上游状态码在风控代码内时的重试间隔"
              >
                <span>风控重试</span>
                <input
                  type="number"
                  min="1"
                  step="1"
                  required
                  value={upstreamRetrySeconds}
                  disabled={busy}
                  onChange={(e) => setUpstreamRetrySeconds(e.target.value)}
                />
                <span>秒</span>
              </label>
              {riskOptions.length > 0 && (
                <fieldset
                  className="provider-risk-options"
                  disabled={busy}
                  title="勾选后，该状态码按风控重试处理；列出的是同域名供应商近 7 天遇到过的上游报错"
                >
                  <legend>风控代码</legend>
                  {riskOptions.map((option) => (
                    <label key={option.status}>
                      <input
                        type="checkbox"
                        checked={checkedCodes.includes(option.status)}
                        onChange={(e) =>
                          setCheckedCodes((old) =>
                            e.target.checked
                              ? [...old, option.status]
                              : old.filter((code) => code !== option.status),
                          )
                        }
                      />
                      <span>
                        {[
                          option.status,
                          option.count ? `${option.count} 次` : "未遇到",
                          limitLabel({ errorBasis: option.basis }),
                        ]
                          .filter(Boolean)
                          .join(" · ")}
                      </span>
                    </label>
                  ))}
                </fieldset>
              )}
              <label
                className="provider-retry-setting provider-risk-codes"
                title="按风控重试处理的上游状态码（400–599），以空格或逗号分隔，最多 16 个，可留空"
              >
                <span>{riskOptions.length ? "其他代码" : "风控代码"}</span>
                <input
                  type="text"
                  inputMode="numeric"
                  spellCheck={false}
                  value={riskCodes}
                  disabled={busy}
                  placeholder="无"
                  onChange={(e) => setRiskCodes(e.target.value)}
                />
              </label>
              {!riskOptions.length && (
                <small className="muted provider-risk-options">
                  暂无已遇到的报错
                </small>
              )}
              <label
                className="provider-retry-setting"
                title="上游返回 429（余额不足等永久拒绝除外），或内容判定为限流的拒绝时的重试间隔"
              >
                <span>限流重试</span>
                <input
                  type="number"
                  min="1"
                  step="1"
                  required
                  value={limitRetrySeconds}
                  disabled={busy}
                  onChange={(e) => setLimitRetrySeconds(e.target.value)}
                />
                <span>秒</span>
              </label>
              <label
                className="provider-retry-setting"
                title="供应商返回 429 等限流后，至少隔这么多秒才向它发出下一个请求：重试和新请求都等待，期间再次限流则重新计时；0 为不限制"
              >
                <span>限流静默</span>
                <input
                  type="number"
                  min="0"
                  max="86400"
                  step="1"
                  required
                  value={limitRestSeconds}
                  disabled={busy}
                  onChange={(e) => setLimitRestSeconds(e.target.value)}
                />
                <span>秒</span>
              </label>
              <label
                className="provider-retry-setting"
                title="其余会重试的失败（连接失败、超时、5xx 等）的重试间隔"
              >
                <span>普通重试</span>
                <input
                  type="number"
                  min="1"
                  step="1"
                  required
                  value={normalRetrySeconds}
                  disabled={busy}
                  onChange={(e) => setNormalRetrySeconds(e.target.value)}
                />
                <span>秒</span>
              </label>
              <label
                className="provider-retry-setting"
                title="风控与限流重试的间隔随该供应商处理中与等待重试的请求数变化的幅度：0 为固定间隔；5 时 3 个以内约为设定值的 1/5，6～7 个为设定值，10 个以上为 5/3；数值越大变化越陡。大于 0 时每次重试的间隔和限流静默另随机延长 0–20%，只延长不缩短"
              >
                <span>动态强度</span>
                <input
                  type="number"
                  min="0"
                  max="10"
                  step="1"
                  required
                  value={retryIntensity}
                  disabled={busy}
                  onChange={(e) => setRetryIntensity(e.target.value)}
                />
                <span>0–10</span>
              </label>
            </div>
          )}
        </div>
      )}
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      <div className="form-actions">
        <button
          type="button"
          className="secondary"
          disabled={busy}
          onClick={cancel}
        >
          取消
        </button>
        <button className="primary" disabled={busy}>
          {busy && <LoaderCircle size={15} className="spin" />}
          {baseline.current === null ? "重试" : "保存"}
        </button>
      </div>
    </form>
  );
}

export default function GatewayDialog({
  clientId,
  connectionMode,
  dialog,
  runtime = dialog.item,
  revision,
  disabled,
  close,
  save,
  update,
  notify,
  onDirtyChange,
}: {
  clientId: ClientId;
  connectionMode: "bearer" | "apiKey";
  dialog: ProviderDialog;
  /** The provider as the gateway reports it now; `dialog.item` is as opened. */
  runtime?: Provider;
  revision: string;
  disabled?: boolean;
  close: () => void;
  save: (p: GatewayEdit, revision: EditRevision) => Promise<void>;
  update: (state: GatewayState) => void;
  notify: (message: string) => void;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  // What a tab edits is the provider as it stood when the tab was opened: an
  // endpoint swap changes it while the dialog stays open.
  const [item, setItem] = useState(dialog.item);
  // DSH maps its fixed model slots in place of a whitelist. A provider that is
  // not saved yet has nothing for the other tabs to edit.
  const tabs: [ProviderTab, string][] = [
    ["api", "API"],
    ...(item && clientId !== "dsh"
      ? ([["whitelist", "白名单"]] as [ProviderTab, string][])
      : []),
    ...(item && (clientId === "dsh" || clientId === "claude")
      ? ([["modelMap", "模型映射"]] as [ProviderTab, string][])
      : []),
    ...(item ? ([["endpoints", "端点"]] as [ProviderTab, string][]) : []),
  ];
  const [tab, setTab] = useState<ProviderTab>(
    tabs.some(([id]) => id === dialog.tab) ? dialog.tab! : "api",
  );
  const [busy, setBusy] = useState(false);
  const dirty = useRef(false);
  const mark = useCallback(
    (value: boolean) => {
      dirty.current = value;
      onDirtyChange?.(value);
    },
    [onDirtyChange],
  );
  const leave = async () => {
    if (
      !busy &&
      (!dirty.current || (await confirmAction("放弃未保存的修改？")))
    )
      close();
  };
  const nav = useRef<HTMLElement>(null);
  // The tab a save started from the tab bar goes on to; a form's own save
  // closes the dialog.
  const onward = useRef<ProviderTab | null>(null);
  const started = useRef(false);
  const [arrival, setArrival] = useState<ProviderTab | null>(null);
  // An effect, so the tab opens on the provider as that save left it.
  useEffect(() => {
    if (arrival === null) return;
    setItem(runtime);
    setTab(arrival);
    setArrival(null);
  }, [arrival]);
  const pick = async (next: ProviderTab) => {
    if (next === tab || busy) return;
    const choice = dirty.current
      ? await confirmSave("有未保存的修改，切换前要保存吗？")
      : "discard";
    if (choice === "cancel") return;
    if (choice === "discard") {
      setItem(runtime);
      setTab(next);
      return;
    }
    onward.current = next;
    started.current = false;
    // The form's own button, so its validation and disabled state apply.
    nav.current
      ?.closest("dialog")
      ?.querySelector<HTMLButtonElement>("form button.primary")
      ?.click();
    if (!started.current) onward.current = null;
  };
  // Forms call `save` synchronously on submit and `finish` once it succeeded.
  const saving =
    (message?: string) => async (p: GatewayEdit, r: EditRevision) => {
      started.current = true;
      try {
        await save(p, r);
      } catch (e) {
        onward.current = null;
        throw e;
      }
      if (message) notify(message);
    };
  const finish = () => {
    if (onward.current === null) return close();
    setArrival(onward.current);
    onward.current = null;
  };
  return (
    <Modal
      title={item ? `编辑供应商 · ${item.name}` : "添加供应商"}
      close={() => void leave()}
      busy={busy}
      className="provider-dialog"
    >
      {tabs.length > 1 && (
        <nav ref={nav} className="settings-tabs" aria-label="供应商设置">
          {tabs.map(([id, name]) => (
            <button
              key={id}
              type="button"
              aria-pressed={tab === id}
              disabled={busy}
              onClick={() => void pick(id)}
            >
              {name}
            </button>
          ))}
        </nav>
      )}
      {tab === "api" && (
        <ProviderApiForm
          clientId={clientId}
          connectionMode={connectionMode}
          dialog={{ kind: "provider", item }}
          revision={revision}
          close={finish}
          cancel={() => void leave()}
          save={saving()}
          onDirtyChange={mark}
          onBusyChange={setBusy}
        />
      )}
      {item &&
        runtime &&
        (tab === "whitelist" || (tab === "modelMap" && clientId === "dsh")) && (
          <ProviderSettings
            provider={item}
            runtime={runtime}
            clientId={clientId}
            revision={revision}
            disabled={disabled}
            close={finish}
            save={saving(
              clientId === "dsh" ? "模型映射已保存" : "白名单已保存",
            )}
            onDirtyChange={mark}
            onBusyChange={setBusy}
          />
        )}
      {item && tab === "modelMap" && clientId === "claude" && (
        <ClaudeModelMapping
          provider={item}
          revision={revision}
          close={finish}
          save={saving("模型映射已保存")}
          onDirtyChange={mark}
          onBusyChange={setBusy}
        />
      )}
      {item && runtime && tab === "endpoints" && (
        <ProviderEndpoints
          provider={item}
          runtime={runtime}
          clientId={clientId}
          revision={revision}
          close={finish}
          save={saving()}
          update={update}
          onDirtyChange={mark}
          onBusyChange={setBusy}
        />
      )}
    </Modal>
  );
}
