import ConnectionTestIcon from "./ConnectionTestIcon";
import Advanced from "./GatewayAdvanced";
import DshProfile, { connectDsh } from "./DshProfile";
import { preferredDshProfile } from "./dsh-profile";
import GatewayKeys from "./GatewayKeys";
import GatewayModels from "./GatewayModels";
import GrokImport from "./GrokImport";
import GatewayDialog, { type ProviderTab } from "./GatewayProviderDialog";
import { subscriptions } from "./subscriptions";
import type { GatewayEdit, ProviderAction } from "./bridge-contracts";
import QuotaRefreshSettings from "./QuotaRefreshSettings";
import ClaudeProfile from "./ClaudeProfile";
import { useInstance } from "./instance";
import ClientSelection, {
  useClientSelection,
  clientName,
} from "./ClientSelection";
import type { ClientId } from "./types";
import { saveGatewayEdit, type EditRevision } from "./gateway-edit";
import { confirmAction } from "./confirmation";
import ProviderControls from "./ProviderControls";
import SortableProviders, { type ProviderCommit } from "./SortableProviders";
import Modal from "./Modal";
import {
  providerRuntimeStatus,
  providerHandoffStatus,
  providerFault,
} from "./provider-status";
import { QuotaInfo, useProviderQuota } from "./Quota";
import {
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  Plus,
  Power,
  Trash2,
  Network,
  Check,
  RotateCcw,
  SquarePen,
  Copy,
} from "lucide-react";
import { command } from "./bridge";
import { errorOf, type GatewayState, type Provider } from "./types";

const ProviderTesting = lazy(() => import("./ProviderTesting"));

type Edit = GatewayEdit;
type Dialog =
  | {
      kind: "testing";
      item: Provider;
    }
  | { kind: "grokImport" }
  | { kind: "dsh" }
  | { kind: "provider"; item?: Provider; tab?: ProviderTab }
  | { kind: "quota"; item: Provider }
  | { kind: "copy"; item: Provider }
  | null;
type GatewayProps = {
  quotaRefreshSeconds?: number;
  notify: (s: string) => void;
  onDirtyChange?: (dirty: boolean) => void;
  onFocusHandled?: () => void;
  focusProvider?: {
    id: string;
    sequence: number;
    clientId: ClientId;
    action?: ProviderAction;
  } | null;
};
export default function Gateway(props: GatewayProps) {
  const [clientId, select] = useClientSelection("main");
  const dirty = useRef(false);
  const changed = useCallback(
    (value: boolean) => {
      dirty.current = value;
      props.onDirtyChange?.(value);
    },
    [props.onDirtyChange],
  );
  useEffect(() => {
    if (props.focusProvider)
      // App has already confirmed discarding drafts for explicit navigation.
      select(props.focusProvider.clientId);
  }, [props.focusProvider?.sequence]);
  return (
    <GatewayContent
      key={clientId}
      {...props}
      clientId={clientId}
      focusProvider={
        props.focusProvider && props.focusProvider.clientId === clientId
          ? props.focusProvider
          : null
      }
      onClientChange={(id) => {
        select(id, dirty.current);
      }}
      onDirtyChange={changed}
    />
  );
}
function GatewayContent({
  quotaRefreshSeconds = 60,
  clientId,
  onClientChange,
  notify,
  onDirtyChange,
  onFocusHandled,
  focusProvider,
}: {
  quotaRefreshSeconds?: number;
  clientId: ClientId;
  onClientChange: (id: ClientId) => void;
  notify: (s: string) => void;
  onDirtyChange?: (dirty: boolean) => void;
  onFocusHandled?: () => void;
  focusProvider?: {
    id: string;
    sequence: number;
    action?: ProviderAction;
  } | null;
}) {
  const { remote, takeover, state: instance } = useInstance();
  // Where else a copy of a provider can go. A server never takes over agy,
  // so its providers stay on the machine they are on.
  const elsewhere =
    clientId === "agy"
      ? []
      : [
          ...(remote ? [{ id: "local", name: "本机" }] : []),
          ...instance.machines.filter((m) => m.id !== instance.active),
        ];
  const [modelsOpen, setModelsOpen] = useState(false);
  const dirtyParts = useRef({ quota: false, advanced: false, dialog: false });
  const markQuota = useCallback(
    (value: boolean) => {
      dirtyParts.current.quota = value;
      onDirtyChange?.(Object.values(dirtyParts.current).some(Boolean));
    },
    [onDirtyChange],
  );
  const markAdvanced = useCallback(
    (value: boolean) => {
      dirtyParts.current.advanced = value;
      onDirtyChange?.(Object.values(dirtyParts.current).some(Boolean));
    },
    [onDirtyChange],
  );
  const markDialog = useCallback(
    (value: boolean) => {
      dirtyParts.current.dialog = value;
      onDirtyChange?.(Object.values(dirtyParts.current).some(Boolean));
    },
    [onDirtyChange],
  );
  const [state, setState] = useState<GatewayState | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [dialog, setDialog] = useState<Dialog>(null);
  const quota = useProviderQuota(
    state?.providers ?? [],
    true,
    "app-visibility",
    clientId,
    quotaRefreshSeconds,
    setError,
  );
  useEffect(() => {
    let disposed = false;
    let sequence = 0;
    const events = subscriptions((e) => setError(errorOf(e).message));
    const refresh = async () => {
      const request = ++sequence;
      try {
        const value = await command("get_gateway", { clientId });
        if (!disposed && request === sequence && value.clientId === clientId)
          setState(value);
      } catch (e) {
        if (!disposed && request === sequence) setError(errorOf(e).message);
      }
    };
    void refresh();
    events.watch("app-visibility", (visible) => {
      if (visible) void refresh();
      else sequence++;
    });
    events.watch("gateway-state", (s) => {
      if (s.clientId === clientId) {
        sequence++;
        setState(s);
      }
    });
    return () => {
      disposed = true;
      events.dispose();
    };
  }, []);
  const focusHandled = useRef<number | null>(null);
  const [dialogVersion, setDialogVersion] = useState(0);
  useEffect(() => {
    if (
      focusProvider &&
      state &&
      focusHandled.current !== focusProvider.sequence
    ) {
      const p = state.providers.find((p) => p.id === focusProvider.id);
      focusHandled.current = focusProvider.sequence;
      if (p) {
        setDialogVersion(focusProvider.sequence);
        // The model rules live in the edit dialog: the whitelist tab, or the
        // model mapping for DSH, which has no whitelist.
        setDialog(
          focusProvider.action === "testing"
            ? { kind: "testing", item: p }
            : {
                kind: "provider",
                item: p,
                tab:
                  focusProvider.action === "edit"
                    ? "api"
                    : clientId === "dsh"
                      ? "modelMap"
                      : "whitelist",
              },
        );
      } else {
        setError("该供应商已不存在，请刷新后重试");
      }
      onFocusHandled?.();
    }
  }, [focusProvider, onFocusHandled, state]);
  const action = async (fn: () => Promise<void>, surfaceError = true) => {
    setBusy(true);
    setError("");
    try {
      await fn();
    } catch (e) {
      if (surfaceError) setError(errorOf(e).message);
      throw e;
    } finally {
      setBusy(false);
    }
  };
  const edit = async (payload: Edit, expected?: EditRevision) => {
    if (!state) return;
    await saveGatewayEdit(clientId, state, payload, expected, setState);
  };
  const run = (fn: () => Promise<void>) => {
    void action(fn).catch(() => {});
  };
  const providerEdit: ProviderCommit = (payload, expected) =>
    action(() => edit(payload, expected), false);
  if (!state)
    return (
      <div className="empty" role="status">
        {error || "正在读取网关…"}
      </div>
    );
  const importDsh = () =>
    run(async () => {
      const preview = await command("preview_dsh_profile", {
        profile: preferredDshProfile(),
      });
      if (!preview.profile || preview.providers?.some((p) => p.error)) {
        setDialog({ kind: "dsh" });
        return;
      }
      setState(await connectDsh(state, preview));
      notify(state.running ? "DSH 已同步" : "DSH 已接入，自动切换已启用");
    });
  const importGrok = () =>
    run(async () => {
      if (state.running) return;
      if (state.grokManaged) {
        setState(
          await command("start_gateway", {
            clientId: "grok",
            expectedRevision: state.revision,
            expectedConfigRevision: state.configRevision,
          }),
        );
      } else {
        const preview = await command("preview_grok_import", {});
        if (!preview.token || preview.error) {
          setDialog({ kind: "grokImport" });
          return;
        }
        setState(
          await command("commit_grok_import", {
            token: preview.token,
            expectedRevision: preview.revision,
          }),
        );
      }
      notify("Grok 已接入");
    });
  // Windows without a PowerShell profile: no agy function was written and
  // there is no command to run by hand, so agy still bypasses the gateway.
  const agyUnwired = (shell: GatewayState["agyShell"]) =>
    !!shell && !shell.profiles.length && !shell.command;
  const agyUnwiredText =
    "未找到 PowerShell 配置文件，agy 仍直连；创建 Microsoft.PowerShell_profile.ps1 后重新启用";
  const toggle = () => {
    if (
      clientId === "grok" &&
      !state.running &&
      !state.grokManaged &&
      !state.recoveryPending
    ) {
      importGrok();
      return;
    }
    if (
      clientId === "dsh" &&
      !state.running &&
      !state.recoveryPending &&
      !state.dshProfile
    ) {
      importDsh();
      return;
    }
    run(async () => {
      const next = await command(
        state.running || state.recoveryPending
          ? "stop_gateway"
          : "start_gateway",
        {
          clientId,
          expectedRevision: state.revision,
          expectedConfigRevision: state.configRevision,
        },
      );
      setState(next);
      notify(
        clientId !== "agy"
          ? `配置已切换，请重新打开 ${clientName(clientId)}`
          : !next.running
            ? "已停用，新运行的 agy 恢复直连"
            : agyUnwired(next.agyShell)
              ? agyUnwiredText
              : "已接入，请打开新的终端运行 agy",
      );
    });
  };
  const choose = (p: Provider) =>
    run(async () => {
      await edit({ op: "select", id: p.id });
      notify(
        state.running
          ? "已切换供应商，新请求立即生效"
          : clientId === "dsh"
            ? "已选择供应商，可在接入面板启用路由"
            : clientId === "agy"
              ? "已选择供应商，启用网关后生效"
              : `文件已切换，请重新打开 ${clientName(clientId)}`,
      );
    });
  const resetProvider = (p: Provider) =>
    run(async () => {
      await edit({ op: "reset", id: p.id });
      notify("已重置熔断");
    });
  const duplicateProvider = (p: Provider) =>
    run(async () => {
      await edit({ op: "duplicateProvider", id: p.id });
      notify("已创建副本");
    });
  const copyProvider = (p: Provider, target: { id: string; name: string }) =>
    run(async () => {
      await command("copy_provider", {
        clientId,
        providerId: p.id,
        instance: instance.active,
        target: target.id,
      });
      notify(`已在「${target.name}」创建副本`);
    });
  const deleteProvider = async (p: Provider) => {
    if (await confirmAction(`删除供应商“${p.name}”？`, "删除"))
      run(() => edit({ op: "deleteProvider", id: p.id }));
  };
  return (
    <section className="gateway-page">
      <div className="gateway-navigation">
        <ClientSelection
          client={clientId}
          select={onClientChange}
          disabled={busy}
        />
        <div className="gateway-client-actions">
          {clientId === "claude" && (
            <ClaudeProfile disabled={busy || !!dialog} notify={notify} />
          )}
          {clientId === "agy" && remote && (
            <span className="muted">服务器版暂不支持接管 agy</span>
          )}
          {clientId === "agy" && !remote && (
            <span
              className="muted"
              title={
                state.agyShell?.command
                  ? `其他 shell 可手动运行：${state.agyShell.command}`
                  : "启用后在 shell 启动文件写入 agy 函数；Antigravity app 不经过网关"
              }
            >
              {!state.agyShell
                ? "仅终端里的 agy 经过网关"
                : state.agyShell.profiles.length
                  ? `agy 函数已写入 ${state.agyShell.profiles.join("、")}`
                  : agyUnwired(state.agyShell)
                    ? agyUnwiredText
                    : "未能写入 shell 启动文件，可手动运行"}
            </span>
          )}
          {clientId === "grok" && !state.running && (
            <button disabled={busy} onClick={importGrok}>
              {state.grokManaged ? "接入当前配置" : "导入并接入"}
            </button>
          )}
          {clientId === "dsh" && (
            <>
              <button disabled={busy} onClick={importDsh}>
                {state.running ? "同步 DSH" : "导入并接入"}
              </button>
              {state.dshProfile && (
                <span className="muted">{state.dshProfile.profile}</span>
              )}
            </>
          )}
        </div>
      </div>
      <div className="gateway-heading gateway-action-row">
        <div className="gateway-route-actions">
          <div className="segmented" aria-label="路由模式">
            <button
              disabled={busy}
              aria-pressed={state.mode === "manual"}
              onClick={() => run(() => edit({ op: "mode", mode: "manual" }))}
            >
              手动
            </button>
            <button
              disabled={busy}
              aria-pressed={state.mode === "auto"}
              onClick={() => run(() => edit({ op: "mode", mode: "auto" }))}
            >
              自动
            </button>
          </div>
          <button
            className={state.running ? "secondary" : "primary"}
            disabled={busy || !takeover(clientId)}
            onClick={toggle}
          >
            <Power size={15} />
            {state.running
              ? "停用"
              : state.recoveryPending
                ? "处理事务"
                : "启用"}
          </button>
        </div>
        <div className="gateway-provider-actions">
          <button
            className="secondary"
            disabled={busy}
            onClick={() => setDialog({ kind: "provider" })}
          >
            <Plus size={15} />
            添加
          </button>
        </div>
      </div>
      {state.configWarning && (
        <div className="inline-error" role="status">
          {state.configWarning}
        </div>
      )}
      {(error || state.error || state.configError) && (
        <div className="banner error" role="alert">
          {error || state.error || state.configError}
        </div>
      )}
      <>
        {state.waitingRequests > 0 && (
          <div className="gateway-waiting">等待 {state.waitingRequests}</div>
        )}
        <div className="provider-list">
          {!state.providers.length && (
            <div className="empty">
              <Network size={24} />
              <strong>尚未添加供应商</strong>
            </div>
          )}
          <SortableProviders
            providers={state.providers}
            revision={state.revision}
            disabled={busy}
            commit={providerEdit}
            report={setError}
            rowClass={(p) =>
              `provider-row ${state.mode === "manual" && state.selected === p.id ? "selected" : ""}`
            }
          >
            {(p, priority, handle, rowBusy) => {
              const selected =
                state.mode === "manual" && state.selected === p.id;
              const status = providerRuntimeStatus(p, state);
              // Left to right in the row's second line.
              const actions: {
                name: string;
                icon: ReactNode;
                run: () => void;
                className?: string;
              }[] = [
                {
                  name: "重置熔断",
                  icon: <RotateCcw size={14} />,
                  run: () => resetProvider(p),
                  className: "provider-reset",
                },
                {
                  name: "连接测试",
                  icon: <ConnectionTestIcon />,
                  run: () => setDialog({ kind: "testing", item: p }),
                },
                // Name, whitelist, model mapping and endpoints are its tabs.
                {
                  name: "编辑 API",
                  icon: <SquarePen size={14} />,
                  run: () => setDialog({ kind: "provider", item: p }),
                },
                {
                  name: "创建副本",
                  icon: <Copy size={14} />,
                  run: () =>
                    elsewhere.length
                      ? setDialog({ kind: "copy", item: p })
                      : duplicateProvider(p),
                },
                {
                  name: "删除",
                  icon: <Trash2 size={14} />,
                  run: () => void deleteProvider(p),
                  className: "provider-delete",
                },
              ];
              return (
                <>
                  <div className="provider-main">
                    {handle}
                    {priority && <span className="priority">P{priority}</span>}
                    <span className="provider-title" title={p.name}>
                      {p.name}
                    </span>
                    <QuotaInfo
                      compact
                      clientId={clientId}
                      provider={p}
                      quota={quota.quotaFor(p)}
                      refresh={() => void quota.refresh(p.id)}
                      details={() => setDialog({ kind: "quota", item: p })}
                    />
                    {status && (
                      <button
                        className="provider-alert"
                        onClick={() =>
                          void command("open_main", {
                            page: "logs",
                            eventId: providerFault(p)?.eventId ?? null,
                            providerId: providerFault(p) ? p.id : null,
                            clientId,
                          }).catch((e) => setError(errorOf(e).message))
                        }
                      >
                        {status}
                      </button>
                    )}
                    {state.configProvider === p.id && !selected && (
                      <span className="provider-binding">配置使用</span>
                    )}
                    {state.mode === "auto" && state.lastSuccessful === p.id && (
                      <span className="provider-binding">最近使用</span>
                    )}

                    <button
                      className="secondary compact"
                      disabled={rowBusy}
                      onClick={() => choose(p)}
                    >
                      {selected ? (
                        <>
                          <Check size={14} />
                          已选择
                        </>
                      ) : (
                        "选择"
                      )}
                    </button>
                  </div>
                  {providerHandoffStatus(p, state) && (
                    <span className="provider-binding provider-handoff">
                      {providerHandoffStatus(p, state)}
                    </span>
                  )}
                  <div className="provider-secondary">
                    <span className="provider-domain" title={p.baseUrl}>
                      {new URL(p.baseUrl).host}
                    </span>
                    <ProviderControls
                      clientId={clientId}
                      provider={p}
                      revision={state.revision}
                      disabled={rowBusy}
                      commit={providerEdit}
                      report={setError}
                    />
                    <div
                      className="provider-actions"
                      role="group"
                      aria-label={`${p.name} 操作`}
                    >
                      {actions.map((a) => (
                        <button
                          key={a.name}
                          type="button"
                          className={`icon-button ${a.className ?? ""}`}
                          aria-label={`${p.name} ${a.name}`}
                          title={a.name}
                          disabled={rowBusy}
                          onClick={a.run}
                        >
                          {a.icon}
                        </button>
                      ))}
                    </div>
                  </div>
                </>
              );
            }}
          </SortableProviders>
        </div>
        <GatewayKeys
          clientId={clientId}
          keys={state.entryKeys ?? []}
          revision={state.revision}
          busy={busy}
          save={edit}
          run={run}
          notify={notify}
        />
        <Advanced
          clientId={clientId}
          onDirtyChange={markAdvanced}
          settings={state.settings}
          listener={state.listener}
          revision={state.revision}
          busy={busy}
          save={async (settings, revision) => {
            await action(() => edit({ op: "settings", settings }, revision));
            notify("网关参数已保存");
          }}
        >
          <div className="gateway-diagnostics">
            <code>{state.address}</code>
            <span>{state.running ? "运行中" : "已关闭"}</span>
            <span>活动连接 {state.activeConnections}</span>
          </div>
          {(clientId === "codex" || clientId === "grok") && (
            <div className="model-catalog-setting">
              <label className="check-label">
                <input
                  type="checkbox"
                  role="switch"
                  checked={state.modelCatalog}
                  disabled={busy}
                  onChange={(e) => {
                    const enabled = e.target.checked;
                    run(async () => {
                      await edit({ op: "modelCatalog", enabled });
                      notify(
                        !enabled
                          ? "已停止接管模型列表"
                          : clientId === "codex"
                            ? "已接管模型列表，重启 Codex 后生效"
                            : "已接管模型列表",
                      );
                    });
                  }}
                />
                接管模型列表
              </label>
              {clientId === "codex" && state.modelCatalog && (
                <label className="model-catalog-source">
                  <span>列表来源</span>
                  <select
                    value={state.modelCatalogOfficial ? "official" : "queue"}
                    disabled={busy}
                    onChange={(e) => {
                      const official = e.target.value === "official";
                      run(async () => {
                        await edit({ op: "modelCatalogSource", official });
                        notify(
                          official
                            ? "已改用官方最新列表，重启 Codex 后生效"
                            : "已改用队列模型，重启 Codex 后生效",
                        );
                      });
                    }}
                  >
                    <option value="queue">队列模型</option>
                    <option value="official">官方最新列表</option>
                  </select>
                </label>
              )}
              {clientId === "codex" &&
                state.modelCatalog &&
                state.modelCatalogOfficial && (
                  <p className="model-catalog-official">
                    <span className="muted">
                      {state.modelCatalogOfficialStatus
                        ? `${state.modelCatalogOfficialStatus.models} 个模型 · 版本 ${state.modelCatalogOfficialStatus.version} · 最近更新 ${new Date(state.modelCatalogOfficialStatus.updatedAt).toLocaleString()}`
                        : "尚未下载"}
                      {state.modelCatalogOfficialError &&
                        ` · ${state.modelCatalogOfficialError}`}
                    </span>
                    <button
                      type="button"
                      className="secondary compact"
                      disabled={busy}
                      onClick={() => {
                        const before =
                          state.modelCatalogOfficialStatus?.version;
                        run(async () => {
                          const next = await command("update_codex_catalog");
                          setState(next);
                          notify(
                            next.modelCatalogOfficialStatus?.version === before
                              ? "官方列表已是最新"
                              : "官方列表已更新，重启 Codex 后生效",
                          );
                        });
                      }}
                    >
                      立即更新
                    </button>
                  </p>
                )}
              {state.modelCatalog && state.modelCatalogError && (
                <p className="form-error" role="alert">
                  {state.modelCatalogError}
                </p>
              )}
            </div>
          )}
          {clientId !== "dsh" && (
            <details
              className="model-list-details"
              id="gateway-model-list"
              open={modelsOpen}
              onToggle={(e) => setModelsOpen(e.currentTarget.open)}
            >
              <summary>模型列表</summary>
              {modelsOpen && (
                <GatewayModels key={clientId} clientId={clientId} />
              )}
            </details>
          )}
          <QuotaRefreshSettings
            seconds={quotaRefreshSeconds}
            onDirtyChange={markQuota}
          />
        </Advanced>
      </>
      {dialog?.kind === "copy" && (
        <Modal title="创建副本" close={() => setDialog(null)} compact>
          <p className="confirmation-message">
            把「{dialog.item.name}」连同令牌和设置复制到：
          </p>
          <div className="copy-targets">
            {[{ id: instance.active, name: "此实例" }, ...elsewhere].map(
              (target) => (
                <button
                  key={target.id}
                  type="button"
                  className="secondary"
                  onClick={() => {
                    const item = dialog.item;
                    setDialog(null);
                    if (target.id === instance.active) duplicateProvider(item);
                    else copyProvider(item, target);
                  }}
                >
                  {target.name}
                </button>
              ),
            )}
          </div>
        </Modal>
      )}
      {dialog?.kind === "dsh" && (
        <DshProfile
          state={state}
          close={() => setDialog(null)}
          update={setState}
          notify={notify}
        />
      )}
      {dialog?.kind === "grokImport" && (
        <GrokImport
          close={() => setDialog(null)}
          update={setState}
          notify={notify}
        />
      )}
      {dialog?.kind === "testing" && (
        <Suspense fallback={null}>
          <ProviderTesting
            key={`${clientId}:${dialog.item.id}:${dialogVersion}`}
            onDirtyChange={markDialog}
            gateway={state}
            provider={
              state.providers.find((p) => p.id === dialog.item.id) ??
              dialog.item
            }
            close={() => setDialog(null)}
          />
        </Suspense>
      )}
      {dialog?.kind === "quota" && (
        <Modal
          compact
          title={`${dialog.item.name} 额度`}
          close={() => setDialog(null)}
        >
          <QuotaInfo
            provider={dialog.item}
            quota={quota.quotaFor(
              state.providers.find((p) => p.id === dialog.item.id) ??
                dialog.item,
            )}
            refresh={() => void quota.refresh(dialog.item.id)}
          />
        </Modal>
      )}
      {dialog?.kind === "provider" && (
        <GatewayDialog
          key={`${clientId}:${dialog.item?.id ?? "new"}:${dialogVersion}`}
          clientId={clientId}
          connectionMode={state.connectionMode}
          dialog={dialog}
          runtime={
            dialog.item &&
            (state.providers.find((p) => p.id === dialog.item?.id) ??
              dialog.item)
          }
          revision={state.revision}
          disabled={busy}
          onDirtyChange={markDialog}
          close={() => setDialog(null)}
          save={edit}
          update={setState}
          notify={notify}
        />
      )}
    </section>
  );
}
