import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowLeftRight,
  Check,
  Download,
  Info,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  RefreshCw,
  Trash2,
  Upload,
  UserRound,
  Users,
} from "lucide-react";
import { command } from "./bridge";
import { subscriptions } from "./subscriptions";
import type {
  AntigravityAccountItem,
  AntigravityAuthState,
} from "./bridge-contracts";
import { AppModal } from "./AppDialogs";
import { errorOf } from "./types";

type Props = {
  notify: (msg: string) => void;
  setError: (msg: string) => void;
  onDirtyChange?: (dirty: boolean) => void;
};

type DialogState =
  | null
  | { type: "rename"; item: AntigravityAccountItem }
  | { type: "delete"; item: AntigravityAccountItem };

export default function AntigravityAccounts({
  notify,
  setError,
  onDirtyChange,
}: Props) {
  const [state, setState] = useState<AntigravityAuthState | null>(null);
  const [busy, setBusy] = useState(false);
  const [dialog, setDialog] = useState<DialogState>(null);

  const [name, setName] = useState("");
  const sequence = useRef(0);

  const [dialogError, setDialogError] = useState("");
  const isDialogDirty = dialog?.type === "rename" && name !== dialog.item.name;
  useEffect(() => {
    onDirtyChange?.(isDialogDirty);
  }, [isDialogDirty, onDirtyChange]);
  useEffect(() => () => onDirtyChange?.(false), [onDirtyChange]);
  const closeDialog = () => {
    setDialog(null);
    setDialogError("");
  };

  const load = useCallback(async () => {
    const id = ++sequence.current;
    try {
      const next = await command("get_antigravity_auth_state");
      if (id === sequence.current) setState(next);
    } catch (e) {
      if (id === sequence.current) setError(errorOf(e).message);
    }
  }, [setError]);

  useEffect(() => {
    void load();
    const events = subscriptions((e) => setError(errorOf(e).message));
    events.watch("antigravity-auth-state", (next) => {
      sequence.current++;
      setState(next);
    });
    return () => {
      sequence.current++;
      events.dispose();
    };
  }, [load, notify, setError]);

  const runAction = async (
    fn: () => Promise<AntigravityAuthState>,
    formatNotice: (next: AntigravityAuthState) => string,
  ) => {
    setBusy(true);
    const id = ++sequence.current;
    setError("");
    setDialogError("");
    try {
      const next = await fn();
      if (id === sequence.current) setState(next);
      const msg = formatNotice(next);
      if (next.relaunchFailed)
        notify(`${msg}；自动重启 Antigravity 失败，请手动打开应用`);
      else if (msg) notify(msg);
      closeDialog();
    } catch (e) {
      if (dialog) setDialogError(errorOf(e).message);
      else setError(errorOf(e).message);
      void load();
    } finally {
      setBusy(false);
    }
  };

  /** Runs an import or export; `run` resolves to nothing when its dialog was dismissed. */
  const transfer = async (run: () => Promise<string | null>) => {
    setBusy(true);
    setError("");
    try {
      const msg = await run();
      if (msg) notify(msg);
    } catch (e) {
      setError(errorOf(e).message);
      void load();
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className="list-tools">
        <h2>
          已保存账号 <span>{state?.accounts.length ?? 0}</span>
        </h2>
        <div className="list-tool-buttons">
          <button
            type="button"
            className="icon-button"
            aria-label="导入 Antigravity 账号"
            title="从 Vela 导出的文件导入账号"
            disabled={busy || !state}
            onClick={() =>
              void transfer(async () => {
                const result = await command("import_antigravity_accounts", {
                  expectedRevision: state!.revision,
                });
                if (!result) return null;
                setState(result.state);
                const skipped = result.skipped
                  ? `${result.skipped} 个账号已存在`
                  : "";
                return result.added
                  ? `已导入 ${result.added} 个账号` +
                      (skipped && `，${skipped}，未改动`)
                  : `没有新账号可导入${skipped && `，${skipped}`}`;
              })
            }
          >
            <Upload size={14} />
          </button>
          <button
            type="button"
            className="icon-button"
            aria-label="导出 Antigravity 账号"
            title="把全部账号导出为一个文件"
            disabled={busy || !state?.accounts.length}
            onClick={() =>
              void transfer(async () => {
                const count = await command("export_antigravity_accounts", {
                  expectedRevision: state!.revision,
                });
                return count == null
                  ? null
                  : `已导出 ${count} 个账号；文件包含登录凭据，请妥善保管`;
              })
            }
          >
            <Download size={14} />
          </button>
          <button
            type="button"
            className="icon-button"
            aria-label="刷新 Antigravity 认证状态"
            disabled={busy}
            onClick={() => void load()}
          >
            <RefreshCw size={14} />
          </button>
        </div>
      </div>

      {!!state?.cliSessions && (
        <div className="banner" role="note">
          <Info size={14} />
          <span>
            有 {state.cliSessions} 个 agy 会话正在运行。agy 与 Antigravity app
            共用同一份登录，每个会话沿用启动时的账号，并在刷新令牌时（约每小时）把它写回。切换账号时
            Vela 会一并关闭这些会话，切换后请重新打开；在 Antigravity
            里重新登录前请先自行退出它们。
          </span>
        </div>
      )}

      <div className="account-list" aria-label="Antigravity 账号列表">
        {!state ? (
          <div className="empty">
            <LoaderCircle className="spin" />
            正在读取 Antigravity 账号…
          </div>
        ) : state.accounts.length === 0 ? (
          <div className="empty">
            <Users size={30} />
            <h3>尚无已保存的 Antigravity 账号</h3>
            <p>在 Antigravity app 内登录后，Vela 会自动识别并加入切换列表。</p>
          </div>
        ) : (
          state.accounts.map((item) => {
            const isEffective = item.current && state.oauthPresent;
            return (
              <div
                key={item.id}
                className={"account-row" + (isEffective ? " selected" : "")}
              >
                <div className="avatar">
                  <UserRound size={20} />
                </div>
                <div className="account-copy">
                  <div className="account-title">
                    <strong title={item.name}>{item.name}</strong>
                  </div>
                  <div className="ag-account-meta">
                    {item.email ? (
                      <span title={item.email}>{item.email}</span>
                    ) : (
                      <span>{item.sourceType}</span>
                    )}
                  </div>
                </div>
                <div className="account-actions">
                  {isEffective ? (
                    <span className="current-badge">
                      <Check size={13} />
                      当前生效
                    </span>
                  ) : (
                    <button
                      type="button"
                      className="switch-button"
                      disabled={busy}
                      aria-label={"切换到 " + item.name}
                      onClick={() =>
                        void runAction(
                          () =>
                            command("switch_antigravity_account", {
                              id: item.id,
                              expectedRevision: state.revision,
                            }),
                          (next) =>
                            (next.appRestarted
                              ? `已切换到 ${item.name} 并自动重启 Antigravity`
                              : `已切换到 ${item.name}`) +
                            (next.cliClosed
                              ? `；已关闭 ${next.cliClosed} 个 agy 会话`
                              : ""),
                        )
                      }
                    >
                      切换
                      <ArrowLeftRight size={13} />
                    </button>
                  )}
                  <details className="account-menu">
                    <summary aria-label={"管理 " + item.name}>
                      <MoreHorizontal size={18} />
                    </summary>
                    <div className="menu-popover">
                      <button
                        type="button"
                        onClick={(e) => {
                          e.currentTarget
                            .closest("details")
                            ?.removeAttribute("open");
                          setName(item.name);
                          setDialog({ type: "rename", item });
                        }}
                      >
                        <Pencil size={14} />
                        重命名
                      </button>
                      <button
                        type="button"
                        className="danger"
                        onClick={(e) => {
                          e.currentTarget
                            .closest("details")
                            ?.removeAttribute("open");
                          setDialog({ type: "delete", item });
                        }}
                      >
                        <Trash2 size={14} />
                        删除
                      </button>
                    </div>
                  </details>
                </div>
              </div>
            );
          })
        )}
      </div>

      {dialog && state && (
        <AppModal
          title={dialog.type === "rename" ? "重命名账号" : "删除账号"}
          onClose={closeDialog}
        >
          {dialog.type === "rename" ? (
            <form
              className="modal-content ag-account-form"
              onSubmit={(e) => {
                e.preventDefault();
                void runAction(
                  () =>
                    command("rename_antigravity_account", {
                      id: dialog.item.id,
                      name,
                      expectedRevision: state.revision,
                    }),
                  () => "已重命名账号",
                );
              }}
            >
              <label className="field">
                新名称
                <input
                  required
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </label>
              {dialogError && (
                <p className="ag-form-error" role="alert">
                  {dialogError}
                </p>
              )}
              <div className="modal-actions">
                <button type="button" onClick={closeDialog}>
                  取消
                </button>
                <button type="submit" className="primary" disabled={busy}>
                  保存
                </button>
              </div>
            </form>
          ) : (
            <div className="modal-content ag-account-form">
              <p>
                确定要从多账号库中删除「{dialog.item.name}
                」吗？正在使用中的账号无法直接删除。
              </p>
              {dialogError && (
                <p className="ag-form-error" role="alert">
                  {dialogError}
                </p>
              )}
              <div className="modal-actions">
                <button type="button" onClick={closeDialog}>
                  取消
                </button>
                <button
                  type="button"
                  className="danger"
                  disabled={busy}
                  onClick={() =>
                    void runAction(
                      () =>
                        command("delete_antigravity_account", {
                          id: dialog.item.id,
                          expectedRevision: state.revision,
                        }),
                      () => "已删除账号",
                    )
                  }
                >
                  删除
                </button>
              </div>
            </div>
          )}
        </AppModal>
      )}
    </>
  );
}
