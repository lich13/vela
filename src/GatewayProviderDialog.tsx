import { useCallback, useEffect, useRef, useState } from "react";
import { Eye, EyeOff, LoaderCircle } from "lucide-react";
import { subscriptions } from "./subscriptions";
import { command } from "./bridge";
import { errorOf, type ClientId, type Provider } from "./types";
import type { GatewayEdit } from "./bridge-contracts";
import type { EditRevision } from "./gateway-edit";
import Modal from "./Modal";

export default function GatewayDialog({
  clientId,
  connectionMode,
  dialog,
  revision,
  close,
  save,
  onDirtyChange,
}: {
  clientId: ClientId;
  connectionMode: "bearer" | "apiKey";
  dialog: { kind: "provider"; item?: Provider };
  close: () => void;
  revision: string;
  save: (p: GatewayEdit, revision: EditRevision) => Promise<void>;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const baseline = useRef<EditRevision>(revision);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [name, setName] = useState(dialog.item ? dialog.item.name : ""),
    [baseUrl, setBaseUrl] = useState(dialog.item?.baseUrl ?? ""),
    [token, setToken] = useState("");
  const [protocol, setProtocol] = useState(
    clientId === "dsh"
      ? "chat_completions"
      : (dialog.item?.protocol ?? "responses"),
  );
  const [showToken, setShowToken] = useState(false);
  const [quotaAccessToken, setQuotaAccessToken] = useState<
    string | undefined
  >();
  const [searchOnly, setSearchOnly] = useState(
    Boolean(dialog.item?.searchOnly),
  );
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
  const [upstreamRetrySeconds, setUpstreamRetrySeconds] = useState(
    String(dialog.item?.upstreamRetrySeconds ?? 5),
  );

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
  const initialDraft = useRef(
    JSON.stringify([
      name,
      baseUrl,
      token,
      protocol,
      quotaAccessToken,
      searchOnly,
      endOnMessageStop,
      filterCountTokens,
      bufferCompleteResponse,
      endlessMode,
      upstreamRetrySeconds,
    ]),
  );
  useEffect(() => {
    onDirtyChange?.(
      JSON.stringify([
        name,
        baseUrl,
        token,
        protocol,
        quotaAccessToken,
        searchOnly,
        endOnMessageStop,
        filterCountTokens,
        bufferCompleteResponse,
        endlessMode,
        upstreamRetrySeconds,
      ]) !== initialDraft.current,
    );
  }, [
    name,
    baseUrl,
    token,
    protocol,
    quotaAccessToken,
    searchOnly,
    endOnMessageStop,
    filterCountTokens,
    bufferCompleteResponse,
    endlessMode,
    upstreamRetrySeconds,
    onDirtyChange,
  ]);
  useEffect(() => () => onDirtyChange?.(false), [onDirtyChange]);
  const title = `${dialog.item ? "编辑" : "添加"}供应商`;
  return (
    <Modal title={title} close={close} busy={busy}>
      <form
        className="gateway-form provider-form"
        onSubmit={(e) => {
          e.preventDefault();
          setError("");
          setBusy(true);
          const draft = {
            id: dialog.item?.id ?? null,
            baseUrl,
            token,
            ...(quotaAccessToken !== undefined ? { quotaAccessToken } : {}),
            ...(!dialog.item ? { name } : {}),
          };
          const payload: GatewayEdit =
            clientId === "grok" || clientId === "dsh"
              ? {
                  op:
                    clientId === "dsh" ? "saveDshProvider" : "saveGrokProvider",
                  ...draft,
                  protocol,
                }
              : {
                  op: "saveProvider",
                  ...draft,
                  ...(clientId === "claude"
                    ? {
                        endOnMessageStop,
                        filterCountTokens,
                        bufferCompleteResponse,
                        endlessMode,
                        upstreamRetrySeconds: endlessMode
                          ? Number(upstreamRetrySeconds)
                          : (dialog.item?.upstreamRetrySeconds ?? 5),
                      }
                    : {}),
                  ...(clientId === "codex" && dialog.item
                    ? { searchOnly }
                    : {}),
                };
          void save(payload, baseline.current)
            .catch((e) => {
              baseline.current = null;
              setError(errorOf(e).message);
            })
            .finally(() => setBusy(false));
        }}
      >
        <>
          {!dialog.item && (
            <label>
              名称
              <input
                type="text"
                maxLength={120}
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="可选"
              />
            </label>
          )}
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
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder={
                clientId === "claude"
                  ? "https://api.example.invalid"
                  : clientId === "dsh"
                    ? "https://api.deepseek.com/v1"
                    : "https://api.example.com/v1"
              }
            />
          </label>
          <label>
            {clientId === "claude"
              ? "ANTHROPIC_AUTH_TOKEN"
              : clientId === "grok" || clientId === "dsh"
                ? "api_key"
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
        {clientId === "codex" && dialog.item && (
          <label className="provider-search-setting">
            <input
              type="checkbox"
              checked={searchOnly}
              disabled={busy}
              onChange={(e) => setSearchOnly(e.target.checked)}
            />
            <span>仅网络搜索</span>
          </label>
        )}
        {clientId === "claude" && (
          <div className="provider-claude-settings">
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
            <div className="provider-endless-setting">
              <label className="provider-search-setting">
                <input
                  type="checkbox"
                  checked={endlessMode}
                  disabled={busy}
                  onChange={(e) => setEndlessMode(e.target.checked)}
                />
                <span>无尽模式</span>
              </label>
              {endlessMode && (
                <label className="provider-retry-setting">
                  <span>重试间隔</span>
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
              )}
            </div>
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
            onClick={close}
          >
            取消
          </button>
          <button className="primary" disabled={busy}>
            {busy && <LoaderCircle size={15} className="spin" />}
            {baseline.current === null ? "重试" : "保存"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
