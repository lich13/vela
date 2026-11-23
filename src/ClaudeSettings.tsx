import { Fragment, useState } from "react";
import {
  Plus,
  RotateCcw,
  Trash2,
  Search,
  RefreshCw,
  TextCursorInput,
  ToggleLeft,
} from "lucide-react";
import { command } from "./bridge";
import { errorOf } from "./types";
import {
  countable,
  fields,
  flag,
  groups,
  get,
  managed,
  patch,
  sensitive,
  type Field,
  type Json,
  type Path,
} from "./claude-settings";
import "./styles/config-visual.css";

/** What a visual control reads and writes; the draft behind it is JSON, TOML or YAML. */
export type Editing = {
  guarded: boolean;
  get: (path: Path) => Json | undefined;
  change: (path: Path, value: Json | undefined) => void;
  /** Values typed as passwords. Structure trees arrive with credentials already hidden. */
  secret: (path: Path) => boolean;
};
/** Shown in place of a control for values a form cannot hold. */
export const sourceOnly = <span className="config-readonly">在原文中编辑</span>;
/** `on` is undefined while the key is absent and the client's default is not known. */
export function Switch({
  label,
  on,
  unset,
  disabled,
  onChange,
}: {
  label: string;
  on: boolean | undefined;
  unset?: boolean;
  disabled?: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <span
      className="config-switch"
      data-state={!unset ? undefined : on === undefined ? "unknown" : "default"}
    >
      <input
        type="checkbox"
        role="switch"
        aria-label={label}
        checked={on === true}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span>{unset ? "默认" : on ? "开启" : "关闭"}</span>
    </span>
  );
}
export function Scalar({
  value,
  path,
  editing,
  label,
  field,
}: {
  value: Json | undefined;
  path: Path;
  editing: Editing;
  label: string;
  /** The curated field behind the row; without one the control follows the value. */
  field?: Field;
}) {
  const kind = field?.kind;
  const locked = editing.guarded && managed(path);
  const secret = editing.secret(path);
  const last = path.at(-1);
  // The text being typed; the file value shows again once the field loses focus.
  const [draft, setDraft] = useState<string | null>(null);
  // A 0 or 1 may be an amount after all, so the row says which and can be turned
  // over; the choice made there stays. Without one the row follows the value,
  // once the field it was typed into loses focus.
  const [turned, setTurned] = useState<boolean | null>(null);
  const state:
    | { on: boolean | undefined; write: (on: boolean) => Json }
    | undefined =
    secret || (field && kind !== "boolean")
      ? undefined
      : value !== undefined
        ? flag(value)
        : field && {
            on: typeof field.initial === "boolean" ? field.initial : undefined,
            write: (on) => flag(field.on ?? true)!.write(on),
          };
  const either = !!state && !field && typeof value !== "boolean";
  const asSwitch =
    !!state &&
    (!either ||
      (turned ??
        (draft === null && typeof last === "string" && !countable(last))));
  const numeric = kind === "number" || typeof value === "number";
  const control =
    state && asSwitch ? (
      <Switch
        label={label}
        on={state.on}
        unset={value === undefined}
        disabled={locked}
        onChange={(on) => editing.change(path, state.write(on))}
      />
    ) : (
      <input
        aria-label={label}
        type={
          secret
            ? "password"
            : kind === "integer" || numeric
              ? "number"
              : "text"
        }
        autoComplete="off"
        spellCheck={false}
        disabled={locked}
        placeholder={
          value !== undefined
            ? undefined
            : field?.initial === undefined || field.initial === ""
              ? "默认"
              : `默认（${field.initial}）`
        }
        value={
          draft ?? (value === undefined || value === null ? "" : String(value))
        }
        step={typeof value === "number" && !Number.isInteger(value) ? "any" : 1}
        min={kind === "integer" ? 0 : field?.min}
        max={field?.max}
        list={kind === "model" ? "claude-models" : undefined}
        onBlur={() => setDraft(null)}
        onChange={(e) => {
          const v = e.target.value;
          if (
            kind === "integer" &&
            v &&
            (!Number.isInteger(Number(v)) || Number(v) < 0)
          )
            return;
          // Only a focused field keeps its own text; anything else shows the file.
          setDraft(document.activeElement === e.target ? v : null);
          if (!numeric) return editing.change(path, v);
          // An empty or half-typed number is not written.
          const n = Number(v);
          if (
            v.trim() === "" ||
            !Number.isFinite(n) ||
            (kind === "number" &&
              (!Number.isInteger(n) ||
                n < (field?.min ?? -Infinity) ||
                n > (field?.max ?? Infinity)))
          )
            return;
          editing.change(path, n);
        }}
      />
    );
  // The wrapper stays while the text comes and goes as a flag, so the field keeps focus.
  if (field || typeof value === "boolean") return control;
  return (
    <span className="config-flag">
      {control}
      {either && (
        <button
          type="button"
          className="icon-button"
          aria-label={`${label} 改为${asSwitch ? "输入框" : "开关"}`}
          title={asSwitch ? "改为输入框" : "改为开关"}
          onClick={() => setTurned(!asSwitch)}
        >
          {asSwitch ? <TextCursorInput size={14} /> : <ToggleLeft size={14} />}
        </button>
      )}
    </span>
  );
}
export function Setting({
  field,
  editing,
  flagUnknown,
}: {
  field: Field;
  editing: Editing;
  flagUnknown?: boolean;
}) {
  const path =
    field.override && editing.get(field.override) !== undefined
      ? field.override
      : field.path;
  const name = field.section ? `${field.section} ${field.label}` : field.label;
  if (field.kind === "list")
    return (
      <StringList path={path} label={name} hint={field.hint} editing={editing} />
    );
  const value = editing.get(path);
  // A table, a list, or a value the structure tree does not spell out.
  const fixed = value !== null && typeof value === "object";
  const unknown =
    field.kind === "select" &&
    value !== undefined &&
    !field.options?.some(([v]) => v === value);
  const unknownText =
    flagUnknown && typeof value === "string" && value.endsWith("_UNSPECIFIED")
      ? "未指定（默认）"
      : flagUnknown
        ? `未知当前值（${String(value)}）`
        : String(value);
  const initial = field.options?.find(([v]) => v === field.initial)?.[1];
  return (
    <div className="setting-row">
      <label htmlFor={path.join(".")}>
        {field.label}
        {field.hint && <small>{field.hint}</small>}
      </label>
      <div className="setting-value">
        {fixed ? (
          sourceOnly
        ) : field.kind === "select" ? (
          <select
            id={path.join(".")}
            aria-label={name}
            value={value === undefined ? "__default__" : String(value)}
            onChange={(e) =>
              editing.change(
                path,
                e.target.value === "__default__" ? undefined : e.target.value,
              )
            }
          >
            <option value="__default__">
              {initial ? `默认（${initial}）` : "默认"}
            </option>
            {unknown && <option value={String(value)}>{unknownText}</option>}
            {field.options?.map(([v, label]) => (
              <option key={v} value={v}>
                {label}
              </option>
            ))}
          </select>
        ) : (
          <Scalar
            value={value}
            path={path}
            editing={editing}
            label={name}
            field={field}
          />
        )}
        <button
          className="icon-button"
          aria-label={`${name}恢复默认`}
          title="恢复默认"
          disabled={value === undefined || fixed}
          onClick={() => editing.change(path, undefined)}
        >
          <RotateCcw size={14} />
        </button>
      </div>
    </div>
  );
}
/** Curated rows of one tab, with a heading wherever the section changes. */
export function Settings({
  fields,
  editing,
  flagUnknown,
}: {
  fields: Field[];
  editing: Editing;
  flagUnknown?: boolean;
}) {
  return fields.map((field, i) => (
    <Fragment key={field.path.join(".")}>
      {field.section && field.section !== fields[i - 1]?.section && (
        <h3 className="settings-section">{field.section}</h3>
      )}
      <Setting field={field} editing={editing} flagUnknown={flagUnknown} />
    </Fragment>
  ));
}
/** Add one item to a list, creating the list when the key is absent. */
function append(editing: Editing, path: Path, item: string) {
  const list = editing.get(path);
  if (Array.isArray(list)) editing.change([...path, list.length], item);
  else editing.change(path, [item]);
}
export function StringList({
  path,
  label,
  hint,
  editing,
}: {
  path: Path;
  label: string;
  hint?: string;
  editing: Editing;
}) {
  const value = editing.get(path),
    entries = Array.isArray(value) ? value : [];
  const [draft, setDraft] = useState("");
  // Anything but a list of single-line strings stays in the source.
  const fixed =
    (value !== undefined && !Array.isArray(value)) ||
    entries.some((entry) => typeof entry !== "string");
  return (
    <section className="settings-list-group">
      <h3>
        {label}
        {hint && <small>{hint}</small>}
      </h3>
      {fixed && sourceOnly}
      {!fixed &&
        entries.map((entry, i) => (
          <div className="settings-list-row" key={i}>
            <Scalar
              value={entry}
              path={[...path, i]}
              editing={editing}
              label={`${label} ${i + 1}`}
              field={{ label, path, kind: "text" }}
            />
            <button
              className="icon-button"
              aria-label={`删除${label} ${i + 1}`}
              onClick={() => editing.change([...path, i], undefined)}
            >
              <Trash2 size={14} />
            </button>
          </div>
        ))}
      {!fixed && (
        <form
          className="settings-list-row"
          onSubmit={(e) => {
            e.preventDefault();
            if (!draft.trim()) return;
            append(editing, path, draft.trim());
            setDraft("");
          }}
        >
          <input
            aria-label={`添加${label}`}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
          />
          <button
            className="icon-button"
            aria-label={`确认添加${label}`}
            disabled={!draft.trim()}
          >
            <Plus size={15} />
          </button>
        </form>
      )}
    </section>
  );
}
/** A rule builder over the allow, ask and deny lists below `path`. */
export function Rules({
  editing,
  path,
  tools,
  bare,
  lists = [],
}: {
  editing: Editing;
  path: Path;
  tools: string[];
  /** Whether a tool name alone is a rule; otherwise every rule is `tool(target)`. */
  bare?: boolean;
  /** Further lists of the same table. */
  lists?: [string, string][];
}) {
  const [tool, setTool] = useState(tools[0]),
    [target, setTarget] = useState(""),
    [action, setAction] = useState("allow");
  return (
    <>
      <section className="settings-list-group">
        <h3>权限规则</h3>
        <form
          className="rule-builder"
          onSubmit={(e) => {
            e.preventDefault();
            const p = [...path, action];
            const old = editing.get(p);
            const rule = target || !bare ? `${tool}(${target})` : tool;
            if (!Array.isArray(old) || !old.includes(rule))
              append(editing, p, rule);
            setTarget("");
          }}
        >
          <select
            aria-label="规则工具"
            value={tool}
            onChange={(e) => setTool(e.target.value)}
          >
            {tools.map((t) => (
              <option key={t}>{t}</option>
            ))}
          </select>
          <input
            aria-label="匹配内容"
            placeholder="匹配内容"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
          />
          <select
            aria-label="规则处理方式"
            value={action}
            onChange={(e) => setAction(e.target.value)}
          >
            <option value="allow">允许</option>
            <option value="ask">询问</option>
            <option value="deny">拒绝</option>
          </select>
          <button className="secondary" disabled={!bare && !target.trim()}>
            添加
          </button>
        </form>
      </section>
      {[["allow", "允许"], ["ask", "询问"], ["deny", "拒绝"], ...lists].map(
        ([key, label]) => (
          <StringList
            key={key}
            path={[...path, key]}
            label={label}
            editing={editing}
          />
        ),
      )}
    </>
  );
}
function AddField({
  path,
  value,
  editing,
}: {
  path: Path;
  value: Json[] | { [key: string]: Json };
  editing: Editing;
}) {
  const [adding, setAdding] = useState(false),
    [name, setName] = useState(""),
    [type, setType] = useState("string");
  const array = Array.isArray(value);
  if (!adding)
    return (
      <button
        className="text-button add-setting"
        onClick={() => setAdding(true)}
      >
        <Plus size={14} />
        {array ? "添加项" : "添加字段"}
      </button>
    );
  return (
    <form
      className="settings-list-row"
      onSubmit={(e) => {
        e.preventDefault();
        if (!array && (!name.trim() || Object.hasOwn(value, name))) return;
        const initial: Record<string, Json> = {
          string: "",
          number: 0,
          boolean: false,
          array: [],
          object: {},
          null: null,
        };
        editing.change([...path, array ? value.length : name], initial[type]);
        setName("");
        setAdding(false);
      }}
    >
      {!array && (
        <input
          aria-label="字段名称"
          placeholder="字段名称"
          required
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      )}
      <select
        aria-label="字段类型"
        value={type}
        onChange={(e) => setType(e.target.value)}
      >
        {[
          ["string", "文本"],
          ["number", "数字"],
          ["boolean", "开关"],
          ["array", "列表"],
          ["object", "对象"],
          ["null", "null"],
        ].map(([v, l]) => (
          <option key={v} value={v}>
            {l}
          </option>
        ))}
      </select>
      <button
        className="secondary"
        disabled={!array && (!name.trim() || Object.hasOwn(value, name))}
      >
        添加
      </button>
      <button
        type="button"
        className="text-button"
        onClick={() => setAdding(false)}
      >
        取消
      </button>
    </form>
  );
}
export function JsonNode({
  path,
  value,
  editing,
  depth = 0,
  hidden,
}: {
  path: Path;
  value: Json;
  editing: Editing;
  depth?: number;
  /** Keys of this node that are left to the source editor. */
  hidden?: string[];
}) {
  const label = String(path.at(-1) ?? "配置");
  const containsManaged =
    editing.guarded &&
    (managed(path) ||
      path.length === 0 ||
      (path.length === 1 && path[0] === "env"));
  if (value && typeof value === "object") {
    const shown = Object.entries(value).filter(
      ([key]) => !hidden?.includes(key),
    );
    return (
      <details className="json-node" open={depth === 0 ? true : undefined}>
        <summary>
          <span>{label}</span>
          <span className="node-count">{shown.length}</span>
          {path.length > 0 && (
            <button
              className="icon-button"
              aria-label={`删除 ${path.join(".")}`}
              disabled={containsManaged}
              onClick={(e) => {
                e.preventDefault();
                editing.change(path, undefined);
              }}
            >
              <Trash2 size={14} />
            </button>
          )}
        </summary>
        <div className="json-children">
          {shown.map(([key, v]) => (
            <JsonNode
              key={key}
              path={[...path, Array.isArray(value) ? Number(key) : key]}
              value={v}
              editing={editing}
              depth={depth + 1}
            />
          ))}
          <AddField path={path} value={value} editing={editing} />
        </div>
      </details>
    );
  }
  return (
    <div className="setting-row tree-scalar">
      <span title={path.join(".")}>{label}</span>
      <div className="setting-value">
        {value === null ? (
          <code>null</code>
        ) : (
          <Scalar
            value={value}
            path={path}
            editing={editing}
            label={path.join(".")}
          />
        )}
        <button
          className="icon-button"
          disabled={containsManaged}
          aria-label={`删除 ${path.join(".")}`}
          onClick={() => editing.change(path, undefined)}
        >
          <Trash2 size={14} />
        </button>
      </div>
    </div>
  );
}
function Environment({ editing }: { editing: Editing }) {
  const [search, setSearch] = useState(""),
    [name, setName] = useState("");
  const env = editing.get(["env"]);
  const rows = env && typeof env === "object" && !Array.isArray(env) ? env : {};
  return (
    <>
      <label className="config-search">
        <Search size={15} />
        <input
          aria-label="搜索环境变量"
          placeholder="搜索变量"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </label>
      <div className="env-list">
        {Object.entries(rows)
          .filter(([key]) => key.toLowerCase().includes(search.toLowerCase()))
          .map(([key, v]) => {
            const field = Object.values(fields)
              .flat()
              .find((f) => f.path[0] === "env" && f.path[1] === key);
            return (
              <div className="environment-row" key={key}>
                <label title={key}>{key}</label>
                {field?.kind === "select" ? (
                  <select
                    aria-label={key}
                    value={String(v)}
                    onChange={(e) =>
                      editing.change(["env", key], e.target.value)
                    }
                  >
                    {!field.options?.some(([value]) => value === v) && (
                      <option value={String(v)}>{String(v)}</option>
                    )}
                    {field.options?.map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </select>
                ) : (
                  <Scalar
                    path={["env", key]}
                    value={v}
                    label={key}
                    editing={editing}
                    field={field}
                  />
                )}
                <button
                  className="icon-button"
                  aria-label={`删除 ${key}`}
                  disabled={editing.guarded && managed(["env", key])}
                  onClick={() => editing.change(["env", key], undefined)}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            );
          })}
      </div>
      <form
        className="settings-list-row"
        onSubmit={(e) => {
          e.preventDefault();
          if (
            /^[A-Za-z_][A-Za-z0-9_]*$/.test(name) &&
            !Object.hasOwn(rows, name)
          ) {
            editing.change(["env", name], "");
            setName("");
          }
        }}
      >
        <input
          aria-label="新环境变量名称"
          placeholder="变量名称"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <button
          className="secondary"
          disabled={
            !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) || Object.hasOwn(rows, name)
          }
        >
          添加
        </button>
      </form>
    </>
  );
}
/** Controls over a JSON draft: every change is one lossless patch of the text. */
export function jsonEditing(
  text: string,
  onChange: (text: string) => void,
  setError: (message: string) => void,
  guarded = false,
): Editing {
  return {
    guarded,
    get: (path) => get(text, path),
    secret: sensitive,
    change: (path, next) => {
      try {
        if (guarded && managed(path))
          throw new Error("网关运行中，连接字段由网关管理");
        onChange(patch(text, path, next));
        setError("");
      } catch (e) {
        setError(errorOf(e).message);
      }
    },
  };
}
export default function ClaudeSettings({
  text,
  onChange,
  guarded,
}: {
  text: string;
  onChange: (text: string) => void;
  guarded: boolean;
}) {
  const [group, setGroup] = useState<(typeof groups)[number]>("模型"),
    [error, setError] = useState(""),
    [models, setModels] = useState<string[]>([]),
    [loading, setLoading] = useState(false);
  let value: Json;
  try {
    value = get(text, [])!;
  } catch {
    return (
      <div className="banner error" role="alert">
        请在 JSON 编辑器中修正配置
      </div>
    );
  }
  const editing = jsonEditing(text, onChange, setError, guarded);
  const loadModels = async () => {
    setLoading(true);
    setError("");
    try {
      const gateway = await command("get_gateway", {
        clientId: "claude",
      });
      const id = gateway.selected ?? gateway.providers[0]?.id;
      if (!id) throw new Error("请先添加 Claude 供应商");
      const list = await command("list_provider_models", {
        clientId: "claude",
        providerId: id,
        force: true,
      });
      setModels(list.models);
    } catch (e) {
      setError(errorOf(e).message);
    } finally {
      setLoading(false);
    }
  };
  return (
    <div className="claude-settings">
      <nav className="settings-tabs" aria-label="Claude 配置分组">
        {groups.map((g) => (
          <button
            key={g}
            aria-pressed={group === g}
            onClick={() => setGroup(g)}
          >
            {g}
          </button>
        ))}
      </nav>
      <div className="settings-content">
        {error && (
          <div className="inline-error" role="alert">
            {error}
          </div>
        )}
        {group === "模型" && (
          <div className="config-group-action">
            <button
              className="text-button"
              disabled={loading}
              onClick={() => void loadModels()}
            >
              <RefreshCw size={14} className={loading ? "spin" : ""} />
              读取模型
            </button>
          </div>
        )}
        <datalist id="claude-models">
          {[...new Set(["opus", "sonnet", "haiku", "opusplan", ...models])].map(
            (m) => (
              <option key={m} value={m} />
            ),
          )}
        </datalist>
        <Settings fields={fields[group] ?? []} editing={editing} />
        {group === "权限" && (
          <Rules
            editing={editing}
            path={["permissions"]}
            tools={["Bash", "Read", "Edit", "Write", "WebFetch", "Agent"]}
            bare
            lists={[["additionalDirectories", "额外访问目录"]]}
          />
        )}
        {group === "环境变量" && <Environment editing={editing} />}
        {group === "高级" && (
          <JsonNode path={[]} value={value} editing={editing} />
        )}
      </div>
    </div>
  );
}
