import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import {
  ArrowLeftRight,
  Check,
  FileInput,
  FileOutput,
  Info,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  RefreshCw,
  Search,
  Trash2,
  UserRound,
  Users,
} from "lucide-react";
import { command } from "./bridge";
import { useDirtyReport } from "./draft-report";
import { useInstance } from "./instance";
import { subscriptions } from "./subscriptions";
import type {
  AntigravityAccountItem,
  AntigravityAuthState,
} from "./bridge-contracts";
import { AppModal } from "./AppDialogs";
import Modal from "./Modal";
import { errorOf } from "./types";

type Props = {
  /** The page title and client switch, which open this page's heading row. */
  heading?: ReactNode;
  notify: (msg: string) => void;
  setError: (msg: string) => void;
  onDirtyChange?: (dirty: boolean) => void;
};

type DialogState =
  | null
  | { type: "rename"; item: AntigravityAccountItem }
  | { type: "delete"; item: AntigravityAccountItem };

export default function AntigravityAccounts({
  heading,
  notify,
  setError,
  onDirtyChange,
}: Props) {
  const { remote } = useInstance();
  const [state, setState] = useState<AntigravityAuthState | null>(null);
  const [busy, setBusy] = useState(false);
  const [dialog, setDialog] = useState<DialogState>(null);
  const [search, setSearch] = useState("");

  const [name, setName] = useState("");
  const sequence = useRef(0);

  const [dialogError, setDialogError] = useState("");
  const isDialogDirty = dialog?.type === "rename" && name !== dialog.item.name;
  useDirtyReport(isDialogDirty, onDirtyChange);
  const closeDialog = () => {
    setDialog(null);
    setDialogError("");
  };
  /** Checked account ids while the export picker is open, otherwise null. */
  const [exportIds, setExportIds] = useState<string[] | null>(null);
  const closeExport = () => {
    setExportIds(null);
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

  /** Runs an import; `run` resolves to nothing when its dialog was dismissed. */
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

  // Accounts removed while the picker is open drop out of the selection.
  const exportable = (state?.accounts ?? [])
    .map((a) => a.id)
    .filter((id) => exportIds?.includes(id));
  const exportSelected = async () => {
    if (!state || !exportable.length) return;
    setBusy(true);
    setError("");
    setDialogError("");
    try {
      const count = await command("export_antigravity_accounts", {
        ids: exportable,
        expectedRevision: state.revision,
      });
      // The save dialog was dismissed: keep the picker open.
      if (count == null) return;
      notify(`已导出 ${count} 个账号；文件包含登录凭据，请妥善保管`);
      closeExport();
    } catch (e) {
      setDialogError(errorOf(e).message);
      void load();
    } finally {
      setBusy(false);
    }
  };

  const shown = (state?.accounts ?? []).filter((a) =>
    (a.name + " " + (a.email ?? ""))
      .toLowerCase()
      .includes(search.toLowerCase()),
  );

  return (
    <>
      <div className="page-heading">
        {heading}
        <div className="account-actions">
          {/* Account files are read and written on this machine only. */}
          {!remote && (
            <div
              className="exchange-button-group"
              role="group"
              aria-label="账号导入导出"
            >
              <button
                type="button"
                className="exchange-button"
                aria-label="导入 Antigravity 账号"
                title="从 Vela 导出的文件导入账号"
                disabled={busy || !state}
                onClick={() =>
                  void transfer(async () => {
                    const result = await command(
                      "import_antigravity_accounts",
                      { expectedRevision: state!.revision },
                    );
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
                <FileInput size={16} />
                导入
              </button>
              <button
                type="button"
                className="exchange-button"
                aria-label="导出 Antigravity 账号"
                title="选择账号并导出为一个文件"
                disabled={busy || !state?.accounts.length}
                onClick={() => {
                  setError("");
                  setExportIds(state!.accounts.map((a) => a.id));
                }}
              >
                <FileOutput size={16} />
                导出
              </button>
            </div>
          )}
          <button
            type="button"
            className="exchange-button"
            aria-label="刷新 Antigravity 认证状态"
            disabled={busy}
            onClick={() => void load()}
          >
            <RefreshCw size={16} />
            刷新
          </button>
        </div>
      </div>

      <div className="list-tools">
        <h2>
          已保存账号 <span>{state?.accounts.length ?? 0}</span>
        </h2>
        <label className="search">
          <Search size={15} />
          <input
            aria-label="搜索账号"
            placeholder="搜索账号"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <kbd>⌕</kbd>
        </label>
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
        ) : shown.length === 0 ? (
          <div className="empty">
            <Users size={30} />
            <h3>
              {search ? "没有找到匹配账号" : "尚无已保存的 Antigravity 账号"}
            </h3>
            <p>
              {search
                ? "尝试其他名称或邮箱"
                : "在 Antigravity app 内登录后，Vela 会自动识别并加入切换列表。"}
            </p>
          </div>
        ) : (
          shown.map((item) => {
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
                              : remote
                                ? "；运行中的 agy 需重新启动后生效"
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

      {exportIds && state && (
        <Modal title="导出账号" close={closeExport} busy={busy}>
          {dialogError && (
            <div className="inline-error" role="alert">
              {dialogError}
            </div>
          )}
          <label className="exchange-select-all">
            <input
              type="checkbox"
              disabled={busy || !state.accounts.length}
              checked={
                state.accounts.length > 0 &&
                exportable.length === state.accounts.length
              }
              onChange={(e) =>
                setExportIds(
                  e.target.checked ? state.accounts.map((a) => a.id) : [],
                )
              }
            />
            全选<span>已选 {exportable.length}</span>
          </label>
          <div className="exchange-list">
            {state.accounts.map((item) => (
              <label key={item.id} className="exchange-row">
                <input
                  type="checkbox"
                  disabled={busy}
                  checked={exportable.includes(item.id)}
                  onChange={(e) =>
                    setExportIds(
                      e.target.checked
                        ? [...exportable, item.id]
                        : exportable.filter((id) => id !== item.id),
                    )
                  }
                />
                <span>
                  <strong title={item.name}>{item.name}</strong>
                  {item.email && <span title={item.email}>{item.email}</span>}
                </span>
              </label>
            ))}
          </div>
          <div className="form-actions exchange-actions">
            <button type="button" disabled={busy} onClick={closeExport}>
              取消
            </button>
            <button
              type="button"
              className="primary"
              disabled={busy || !exportable.length}
              onClick={() => void exportSelected()}
            >
              导出所选
            </button>
          </div>
        </Modal>
      )}
    </>
  );
}
