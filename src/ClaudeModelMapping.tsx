import { useEffect, useId, useRef, useState } from "react";
import { ArrowRight, Plus, RefreshCw, Trash2 } from "lucide-react";
import { command } from "./bridge";
import type { GatewayEdit } from "./bridge-contracts";
import { confirmAction } from "./confirmation";
import type { EditRevision } from "./gateway-edit";
import Modal from "./Modal";
import { errorOf, type Provider } from "./types";

const MANUAL = "__manual_model__";
type Row = { key: number; from: string; to: string; manual: boolean };

/** The first mapping that cannot be saved, as a message naming its row. */
function problem(pairs: [string, string][]) {
  for (const [index, [from, to]] of pairs.entries()) {
    const row = `映射 ${index + 1}`;
    if (!from || !to) return `${row} 未填写完整，请补全或删除`;
    if (
      [from, to].some(
        (id) =>
          new TextEncoder().encode(id).length > 256 ||
          /[\x00-\x1f\x7f*]/.test(id),
      )
    )
      return `${row} 请输入完整模型 ID，不支持通配符`;
    if (from === to) return `${row} 的请求模型与上游模型相同，无需映射`;
    const first = pairs.findIndex(([other]) => other === from);
    if (first !== index)
      return `${row} 与映射 ${first + 1} 的请求模型重复`;
  }
  return "";
}

export default function ClaudeModelMapping({
  provider,
  revision,
  save,
  close,
  onDirtyChange,
}: {
  provider: Provider;
  revision: string;
  save: (edit: GatewayEdit, revision: EditRevision) => Promise<void>;
  close: () => void;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [rows, setRows] = useState<Row[]>(() =>
    Object.entries(provider.modelMap).map(([from, to], key) => ({
      key,
      from,
      to,
      manual: false,
    })),
  );
  const nextKey = useRef(rows.length);
  // The provider's own catalog: what an upstream model is picked from.
  const [models, setModels] = useState<string[]>([]);
  // Every model the Claude gateway lists: what a client may ask for.
  const [known, setKnown] = useState<string[]>([]);
  const [catalogError, setCatalogError] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const baseline = useRef<EditRevision>(revision);
  const draft = JSON.stringify(rows.map(({ from, to }) => [from, to]));
  const initial = useRef(draft);
  const dirty = draft !== initial.current;
  const generation = useRef(0);
  const suggestions = useId();
  async function load(force: boolean) {
    const seq = ++generation.current;
    setLoading(true);
    try {
      const result = await command("list_provider_models", {
        clientId: "claude",
        providerId: provider.id,
        force,
      });
      if (seq === generation.current) {
        setModels(result.models);
        setCatalogError(result.error ?? "");
      }
    } catch (e) {
      if (seq === generation.current) setCatalogError(errorOf(e).message);
    } finally {
      if (seq === generation.current) setLoading(false);
    }
  }
  useEffect(() => {
    void load(false);
    return () => {
      generation.current++;
    };
  }, [provider.id]);
  useEffect(() => {
    let gone = false;
    void command("get_gateway_models", { clientId: "claude" })
      .then((view) => {
        if (!gone) setKnown(view.models.map((model) => model.id));
      })
      // Suggestions only: the request model stays typeable without them.
      .catch(() => {});
    return () => {
      gone = true;
    };
  }, []);
  useEffect(() => {
    onDirtyChange?.(dirty);
    return () => onDirtyChange?.(false);
  }, [dirty, onDirtyChange]);
  const leave = async () => {
    if (!dirty || (await confirmAction("放弃未保存的模型映射？"))) close();
  };
  const change = (key: number, patch: Partial<Row>) =>
    setRows((old) =>
      old.map((row) => (row.key === key ? { ...row, ...patch } : row)),
    );
  return (
    <Modal
      title={`${provider.name} 模型映射`}
      close={() => void leave()}
      busy={busy}
      className="model-mapping-modal"
    >
      <form
        className="gateway-form model-mapping"
        onSubmit={(e) => {
          e.preventDefault();
          const pairs = rows.map(
            (row) => [row.from.trim(), row.to.trim()] as [string, string],
          );
          const issue = problem(pairs);
          if (issue) {
            setError(issue);
            return;
          }
          setBusy(true);
          setError("");
          void save(
            {
              op: "modelMapProvider",
              id: provider.id,
              mapping: Object.fromEntries(pairs),
            },
            baseline.current,
          )
            .then(close)
            .catch((e) => {
              baseline.current = null;
              setError(errorOf(e).message);
            })
            .finally(() => setBusy(false));
        }}
      >
        {rows.length > 0 && (
          <div className="model-mapping-list">
            <div className="model-mapping-row model-mapping-head">
              <span>请求模型</span>
              <span />
              <span>上游模型</span>
            </div>
            {rows.map((row, index) => (
              <div className="model-mapping-row" key={row.key}>
                <input
                  aria-label={`映射 ${index + 1} 请求模型`}
                  list={suggestions}
                  value={row.from}
                  maxLength={256}
                  spellCheck={false}
                  disabled={busy}
                  placeholder="完整模型 ID"
                  onChange={(e) => change(row.key, { from: e.target.value })}
                />
                <ArrowRight size={14} aria-hidden="true" />
                <select
                  aria-label={`映射 ${index + 1} 上游模型`}
                  disabled={busy}
                  value={row.manual ? MANUAL : row.to}
                  onChange={(e) =>
                    change(
                      row.key,
                      e.target.value === MANUAL
                        ? { manual: true }
                        : { manual: false, to: e.target.value },
                    )
                  }
                >
                  <option value="">选择上游模型</option>
                  {[...new Set([row.to, ...models])]
                    .filter(Boolean)
                    .map((model) => (
                      <option key={model} value={model}>
                        {model}
                      </option>
                    ))}
                  <option value={MANUAL}>手动填写…</option>
                </select>
                <button
                  type="button"
                  className="icon-button"
                  aria-label={`删除映射 ${index + 1}`}
                  title="删除"
                  disabled={busy}
                  onClick={() =>
                    setRows((old) => old.filter((r) => r.key !== row.key))
                  }
                >
                  <Trash2 size={14} />
                </button>
                {row.manual && (
                  <input
                    className="model-mapping-custom"
                    aria-label={`映射 ${index + 1} 自定义模型 ID`}
                    value={row.to}
                    maxLength={256}
                    spellCheck={false}
                    disabled={busy}
                    placeholder="完整模型 ID"
                    onChange={(e) => change(row.key, { to: e.target.value })}
                  />
                )}
              </div>
            ))}
          </div>
        )}
        <datalist id={suggestions}>
          {[...new Set([...known, ...models])].map((model) => (
            <option key={model} value={model} />
          ))}
        </datalist>
        <p className="muted model-mapping-note">未映射的模型按原名转发</p>
        <div className="model-mapping-actions">
          <button
            type="button"
            className="text-button"
            disabled={busy}
            onClick={() =>
              setRows((old) => [
                ...old,
                { key: nextKey.current++, from: "", to: "", manual: false },
              ])
            }
          >
            <Plus size={14} />
            添加映射
          </button>
          <button
            type="button"
            className="text-button"
            disabled={loading || busy}
            onClick={() => void load(true)}
          >
            <RefreshCw size={14} className={loading ? "spin" : ""} />
            刷新模型列表
          </button>
        </div>
        {catalogError && (
          <p className="form-error" role="status">
            {catalogError}
          </p>
        )}
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
        <footer className="modal-actions">
          <button
            type="button"
            className="secondary"
            disabled={busy}
            onClick={() => void leave()}
          >
            取消
          </button>
          <button className="primary" disabled={busy || !dirty}>
            保存
          </button>
        </footer>
      </form>
    </Modal>
  );
}
