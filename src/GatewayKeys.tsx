import { useState } from "react";
import { Copy, Pencil, Plus, Trash2 } from "lucide-react";
import { command } from "./bridge";
import type { GatewayEdit } from "./bridge-contracts";
import { confirmAction } from "./confirmation";
import type { EditRevision } from "./gateway-edit";
import { currentInstance, useInstance } from "./instance";
import Modal from "./Modal";
import { errorOf, type ClientId, type EntryKey } from "./types";

type Save = (edit: GatewayEdit, revision?: EditRevision) => Promise<void>;

export default function GatewayKeys({
  clientId,
  keys,
  revision,
  busy,
  save,
  run,
  notify,
}: {
  clientId: ClientId;
  keys: EntryKey[];
  revision: string;
  busy: boolean;
  save: Save;
  /** Runs a page action: busy while it lasts, its failure on the page. */
  run: (fn: () => Promise<void>) => void;
  notify: (text: string) => void;
}) {
  const { supports, takeover } = useInstance();
  const [editing, setEditing] = useState<Partial<EntryKey> | null>(null);
  // An instance from before there were keys has none to copy, and a client it
  // cannot take over has no gateway to hand them out for.
  if (!supports("export_gateway_key") || !takeover(clientId)) return null;
  const copy = (key: EntryKey) =>
    run(async () => {
      await command("copy_gateway_key", {
        clientId,
        instance: currentInstance(),
        keyId: key.id,
      });
      notify("Key 已复制");
    });
  const remove = async (key: EntryKey) => {
    if (
      await confirmAction(
        `删除 Key「${key.name}」？使用它的请求会立即被拒绝，已有用量记录保留。`,
        "删除",
      )
    )
      run(() => save({ op: "deleteEntryKey", id: key.id }));
  };
  return (
    <>
      <details className="advanced gateway-keys">
        <summary>访问 Key</summary>
        <div className="policy-parameters">
          <div className="policy-parameter">
            <span>本机 Key</span>
            <span className="policy-unit gateway-key-side">默认</span>
          </div>
          {keys.map((key) => (
            <div className="policy-parameter" key={key.id}>
              <span className="gateway-key-name" title={key.name}>
                {key.name}
              </span>
              <div className="gateway-key-side">
                <button
                  type="button"
                  className="icon-button"
                  disabled={busy}
                  aria-label={`复制 ${key.name}`}
                  onClick={() => copy(key)}
                >
                  <Copy size={14} />
                </button>
                <button
                  type="button"
                  className="icon-button"
                  disabled={busy}
                  aria-label={`重命名 ${key.name}`}
                  onClick={() => setEditing(key)}
                >
                  <Pencil size={14} />
                </button>
                <button
                  type="button"
                  className="icon-button"
                  disabled={busy}
                  aria-label={`删除 ${key.name}`}
                  onClick={() => void remove(key)}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            </div>
          ))}
        </div>
        <button
          type="button"
          className="text-button gateway-key-add"
          disabled={busy}
          onClick={() => setEditing({})}
        >
          <Plus size={14} />
          添加 Key
        </button>
      </details>
      {/* Outside the block: `.advanced form` would pad the dialog's form. */}
      {editing && (
        <KeyDialog
          initial={editing}
          revision={revision}
          save={save}
          close={() => setEditing(null)}
        />
      )}
    </>
  );
}

function KeyDialog({
  initial,
  revision,
  save,
  close,
}: {
  initial: Partial<EntryKey>;
  revision: string;
  save: Save;
  close: () => void;
}) {
  const [name, setName] = useState(initial.name ?? "");
  // What the dialog opened on; null after a failed save, so that the retry
  // writes against whatever is there by then.
  const [expected, setExpected] = useState<EditRevision>(revision);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  return (
    <Modal
      compact
      title={initial.id ? "重命名 Key" : "添加 Key"}
      close={close}
      busy={busy}
    >
      <form
        className="gateway-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (busy) return;
          const value = name.trim();
          if (!value) {
            setError("请填写名称");
            return;
          }
          // A name saved as it is changes nothing.
          if (value === initial.name) {
            close();
            return;
          }
          setBusy(true);
          setError("");
          void save(
            {
              op: "saveEntryKey",
              ...(initial.id ? { id: initial.id } : {}),
              name: value,
            },
            expected,
          )
            .then(close)
            .catch((e) => {
              setExpected(null);
              setError(errorOf(e).message);
            })
            .finally(() => setBusy(false));
        }}
      >
        <label>
          名称
          <input
            value={name}
            maxLength={120}
            disabled={busy}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
        <div className="modal-actions">
          <button type="button" disabled={busy} onClick={close}>
            取消
          </button>
          <button className="primary" disabled={busy}>
            {busy ? "保存中…" : expected === null ? "重试" : "保存"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
