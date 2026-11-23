import { useEffect, useId, useRef, useState } from "react";
import {
  ArrowRight,
  ChevronDown,
  ChevronRight,
  Plus,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { command } from "./bridge";
import type { GatewayEdit } from "./bridge-contracts";
import { confirmAction } from "./confirmation";
import type { EditRevision } from "./gateway-edit";
import Modal from "./Modal";
import { errorOf, type Provider } from "./types";

const MANUAL = "__manual_model__";
const FAMILY_PREFIX = "family:";
const FAMILIES = [
  ["fable", "Fable"],
  ["opus", "Opus"],
  ["sonnet", "Sonnet"],
  ["haiku", "Haiku"],
] as const;
type Family = (typeof FAMILIES)[number][0];
type Target = { to: string; manual: boolean };
type Row = Target & { key: number; from: string };

/**
 * The family of a model a Claude client asks for, judged as the gateway does
 * (`model_family` in src-tauri/src/gateway/model.rs): an alias, or a Claude
 * model ID carrying the family word, with or without a `[1m]` suffix.
 */
export function modelFamily(model: string): Family | null {
  const lower = model.trim().toLowerCase();
  const open = lower.indexOf("[");
  const id = open >= 0 && lower.endsWith("]") ? lower.slice(0, open) : lower;
  if (id === "opusplan") return "opus";
  const named = (word: string) =>
    FAMILIES.find(([family]) => family === word)?.[0] ?? null;
  const alias = named(id);
  if (alias) return alias;
  const start = /(?:^|[^a-z0-9])claude-/.exec(id);
  if (!start) return null;
  for (const word of id
    .slice(start.index + start[0].length - "claude-".length)
    .split(/[^a-z0-9]/)) {
    const family = named(word);
    if (family) return family;
  }
  return null;
}

const unusable = (id: string) =>
  new TextEncoder().encode(id).length > 256 || /[\x00-\x1f\x7f*]/.test(id);

/** The first mapping that cannot be saved, as a message naming its row. */
function problem(families: [string, string][], pairs: [string, string][]) {
  for (const [label, to] of families)
    if (unusable(to)) return `${label} 请输入完整模型 ID，不支持通配符`;
  for (const [index, [from, to]] of pairs.entries()) {
    const row = `映射 ${index + 1}`;
    if (!from || !to) return `${row} 未填写完整，请补全或删除`;
    if (unusable(from) || unusable(to) || from.startsWith(FAMILY_PREFIX))
      return `${row} 请输入完整模型 ID，不支持通配符`;
    if (from === to) return `${row} 的请求模型与上游模型相同，无需映射`;
    const first = pairs.findIndex(([other]) => other === from);
    if (first !== index)
      return `${row} 与映射 ${first + 1} 的请求模型重复`;
  }
  return "";
}

const familyKey = (family: Family) => `${FAMILY_PREFIX}${family}`;

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
  const [families, setFamilies] = useState(
    () =>
      Object.fromEntries(
        FAMILIES.map(([family]) => [
          family,
          { to: provider.modelMap[familyKey(family)] ?? "", manual: false },
        ]),
      ) as Record<Family, Target>,
  );
  const [rows, setRows] = useState<Row[]>(() =>
    Object.entries(provider.modelMap)
      .filter(
        ([from]) => !FAMILIES.some(([family]) => from === familyKey(family)),
      )
      .map(([from, to], key) => ({ key, from, to, manual: false })),
  );
  const nextKey = useRef(rows.length);
  const [exactOpen, setExactOpen] = useState(rows.length > 0);
  // The provider's own catalog: what an upstream model is picked from.
  const [models, setModels] = useState<string[]>([]);
  // Every model the Claude gateway lists: what a client may ask for.
  const [known, setKnown] = useState<string[]>([]);
  // What Claude clients did ask for lately; null until that is known.
  const [requested, setRequested] = useState<
    { model: string; count: number }[] | null
  >(null);
  const [catalogError, setCatalogError] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const baseline = useRef<EditRevision>(revision);
  const draft = JSON.stringify([
    FAMILIES.map(([family]) => families[family].to),
    rows.map(({ from, to }) => [from, to]),
  ]);
  const initial = useRef(draft);
  const dirty = draft !== initial.current;
  const generation = useRef(0);
  const suggestions = useId();
  const exactPanel = useId();
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
    void command("get_claude_requested_models")
      .then((list) => {
        if (!gone && Array.isArray(list)) setRequested(list);
      })
      // A hint only: the families map the same without it.
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
  const changeFamily = (family: Family, patch: Partial<Target>) =>
    setFamilies((old) => ({ ...old, [family]: { ...old[family], ...patch } }));
  const options = (current: string) =>
    [...new Set([current, ...models])].filter(Boolean).map((model) => (
      <option key={model} value={model}>
        {model}
      </option>
    ));
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
          const chosen = FAMILIES.map(
            ([family, label]) =>
              [family, label, families[family].to.trim()] as const,
          ).filter(([, , to]) => to);
          const pairs = rows.map(
            (row) => [row.from.trim(), row.to.trim()] as [string, string],
          );
          const issue = problem(
            chosen.map(([, label, to]) => [label, to]),
            pairs,
          );
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
              mapping: Object.fromEntries([
                ...chosen.map(([family, , to]) => [familyKey(family), to]),
                ...pairs,
              ]),
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
        <div className="model-mapping-families">
          {FAMILIES.map(([family, label]) => {
            const target = families[family];
            const asked = requested?.filter(
              (entry) => modelFamily(entry.model) === family,
            );
            return (
              <div className="model-mapping-family" key={family}>
                <span className="model-mapping-family-name">{label}</span>
                <ArrowRight size={14} aria-hidden="true" />
                <select
                  aria-label={`${label} 上游模型`}
                  disabled={busy}
                  value={target.manual ? MANUAL : target.to}
                  onChange={(e) =>
                    changeFamily(
                      family,
                      e.target.value === MANUAL
                        ? { manual: true }
                        : { manual: false, to: e.target.value },
                    )
                  }
                >
                  <option value="">不映射</option>
                  {options(target.to)}
                  <option value={MANUAL}>手动填写…</option>
                </select>
                {target.manual && (
                  <input
                    className="model-mapping-custom"
                    aria-label={`${label} 自定义模型 ID`}
                    value={target.to}
                    maxLength={256}
                    spellCheck={false}
                    disabled={busy}
                    placeholder="完整模型 ID，留空则不映射"
                    onChange={(e) =>
                      changeFamily(family, { to: e.target.value })
                    }
                  />
                )}
                {asked && (
                  <p
                    className="muted model-mapping-asked"
                    data-testid={`asked-${family}`}
                  >
                    {asked.length
                      ? `Claude Code 近期实际请求：${asked
                          .map((entry) => `${entry.model} ×${entry.count}`)
                          .join("、")}`
                      : "近 7 天未请求"}
                  </p>
                )}
              </div>
            );
          })}
        </div>
        <button
          type="button"
          className="text-button model-mapping-toggle"
          aria-expanded={exactOpen}
          aria-controls={exactPanel}
          onClick={() => setExactOpen((open) => !open)}
        >
          {exactOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          按模型 ID 单独指定
          {rows.length > 0 && `（${rows.length}）`}
        </button>
        {exactOpen && (
          <div className="model-mapping-exact" id={exactPanel}>
            <p className="muted model-mapping-note">
              单独指定的模型优先于所属系列的映射
            </p>
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
                      onChange={(e) =>
                        change(row.key, { from: e.target.value })
                      }
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
                      {options(row.to)}
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
                        onChange={(e) =>
                          change(row.key, { to: e.target.value })
                        }
                      />
                    )}
                  </div>
                ))}
              </div>
            )}
            <datalist id={suggestions}>
              {[
                ...new Set([
                  ...(requested ?? []).map((entry) => entry.model),
                  ...known,
                  ...models,
                ]),
              ].map((model) => (
                <option key={model} value={model} />
              ))}
            </datalist>
          </div>
        )}
        <p className="muted model-mapping-note">未映射的模型按原名转发</p>
        <div className="model-mapping-actions">
          <button
            type="button"
            className="text-button"
            disabled={busy}
            onClick={() => {
              setExactOpen(true);
              setRows((old) => [
                ...old,
                { key: nextKey.current++, from: "", to: "", manual: false },
              ]);
            }}
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
