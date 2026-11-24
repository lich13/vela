import { useEffect, useState } from "react";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { command } from "./bridge";
import type { Machine } from "./bridge-contracts";
import { confirmAction } from "./confirmation";
import { setInstanceState, useInstance } from "./instance";
import { reloadInstances } from "./InstanceSwitch";
import Modal from "./Modal";
import { errorOf } from "./types";

type Draft = {
  id: string | null;
  name: string;
  sshHost: string;
  fallbackHost: string;
  useSudo: boolean;
};
// The alias becomes an ssh argument: no option-like or shell-like values.
const validAlias = (value: string) =>
  /^[A-Za-z0-9._-]+$/.test(value) && !value.startsWith("-");
const aliasRule = "只能包含字母、数字、点、下划线和连字符，且不能以连字符开头";

/** The servers this app can control over SSH: add, edit, test and delete. */
export default function InstanceManager({ close }: { close: () => void }) {
  const { state } = useInstance();
  const [draft, setDraft] = useState<Draft | null>(null);
  /** What is in flight, as the status line shows it. */
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [tested, setTested] = useState("");
  useEffect(() => {
    void reloadInstances().catch((e) => setError(errorOf(e).message));
  }, []);
  const run = async (label: string, fn: () => Promise<void>) => {
    setBusy(label);
    setError("");
    setTested("");
    try {
      await fn();
    } catch (e) {
      setError(errorOf(e).message);
      if (errorOf(e).code === "CONFLICT")
        await reloadInstances().catch(() => {});
    } finally {
      setBusy("");
    }
  };
  const edit = (next: Draft | null) => {
    setDraft(next);
    setError("");
    setTested("");
  };
  /** The connection fields of a draft that passes the alias rule. */
  const target = (d: Draft) => {
    const sshHost = d.sshHost.trim();
    const fallbackHost = d.fallbackHost.trim() || null;
    if (!validAlias(sshHost)) throw new Error(`SSH 主机别名${aliasRule}`);
    if (fallbackHost && !validAlias(fallbackHost))
      throw new Error(`备用别名${aliasRule}`);
    return { sshHost, fallbackHost, useSudo: d.useSudo };
  };
  const remove = (machine: Machine) =>
    run("正在删除…", async () => {
      if (
        machine.id === state.active &&
        !(await confirmAction(
          `「${machine.name}」正在使用中，删除后会切回本机。`,
          "删除",
        ))
      )
        return;
      setInstanceState(
        await command("delete_instance", {
          id: machine.id,
          expectedRevision: state.revision,
        }),
      );
    });
  if (!draft)
    return (
      <Modal title="管理实例" close={close} busy={!!busy} compact>
        {state.machines.length ? (
          <ul className="instance-machines" aria-label="服务器">
            {state.machines.map((machine) => (
              <li key={machine.id}>
                <div>
                  <strong>{machine.name}</strong>
                  <span>
                    {machine.sshHost}
                    {machine.fallbackHost && ` · 备用 ${machine.fallbackHost}`}
                  </span>
                </div>
                <button
                  type="button"
                  className="icon-button"
                  aria-label={`编辑 ${machine.name}`}
                  disabled={!!busy}
                  onClick={() =>
                    edit({
                      ...machine,
                      fallbackHost: machine.fallbackHost ?? "",
                    })
                  }
                >
                  <Pencil size={14} />
                </button>
                <button
                  type="button"
                  className="icon-button"
                  aria-label={`删除 ${machine.name}`}
                  disabled={!!busy}
                  onClick={() => void remove(machine)}
                >
                  <Trash2 size={14} />
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="confirmation-message">
            还没有服务器。添加后可以经 SSH 查看和操作服务器上的 Vela。
          </p>
        )}
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
        <div className="modal-actions">
          <button
            type="button"
            className="primary"
            disabled={!!busy}
            onClick={() =>
              edit({
                id: null,
                name: "",
                sshHost: "",
                fallbackHost: "",
                useSudo: false,
              })
            }
          >
            <Plus size={15} />
            添加服务器
          </button>
        </div>
      </Modal>
    );
  const change = (patch: Partial<Draft>) => {
    setDraft({ ...draft, ...patch });
    setTested("");
  };
  return (
    <Modal
      title={draft.id ? "编辑服务器" : "添加服务器"}
      close={close}
      busy={!!busy}
      compact
    >
      <form
        className="gateway-form instance-form"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          void run("正在保存…", async () => {
            const name = draft.name.trim();
            if (!name) throw new Error("请填写名称");
            setInstanceState(
              await command("save_instance", {
                expectedRevision: state.revision,
                machine: { id: draft.id, name, ...target(draft) },
              }),
            );
            setDraft(null);
          });
        }}
      >
        <label>
          名称
          <input
            value={draft.name}
            maxLength={64}
            disabled={!!busy}
            onChange={(e) => change({ name: e.target.value })}
          />
        </label>
        <label>
          SSH 主机别名
          <input
            value={draft.sshHost}
            disabled={!!busy}
            spellCheck={false}
            autoCapitalize="off"
            onChange={(e) => change({ sshHost: e.target.value })}
          />
          <small>填 ~/.ssh/config 里的 Host 名称</small>
        </label>
        <label>
          备用别名
          <input
            value={draft.fallbackHost}
            placeholder="可留空"
            disabled={!!busy}
            spellCheck={false}
            autoCapitalize="off"
            onChange={(e) => change({ fallbackHost: e.target.value })}
          />
        </label>
        <label className="check-label instance-sudo">
          <input
            type="checkbox"
            checked={draft.useSudo}
            disabled={!!busy}
            onChange={(e) => change({ useSudo: e.target.checked })}
          />
          使用 sudo
          <small>登录用户不是 root 时勾选</small>
        </label>
        {(busy || tested) && <p role="status">{busy || tested}</p>}
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
        <div className="modal-actions">
          <button
            type="button"
            className="secondary"
            disabled={!!busy}
            onClick={() =>
              void run("正在测试连接…", async () => {
                const { version } = await command(
                  "test_instance",
                  target(draft),
                );
                setTested(`连接成功，服务器版本 v${version}`);
              })
            }
          >
            测试连接
          </button>
          <button type="button" disabled={!!busy} onClick={() => edit(null)}>
            取消
          </button>
          <button className="primary" disabled={!!busy}>
            保存
          </button>
        </div>
      </form>
    </Modal>
  );
}
