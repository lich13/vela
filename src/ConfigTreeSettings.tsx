import { useEffect, useRef, useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { command } from "./bridge";
import { errorOf } from "./types";
import type { Field, Path } from "./claude-settings";
import {
  Scalar,
  Setting,
  Settings,
  sourceOnly,
  type Editing,
} from "./ClaudeSettings";
import { credentialKey, type NamedTables } from "./config-fields";
import {
  nodeAt,
  plain,
  type ConfigTreeFormat,
  type ConfigTreeNode,
  type ConfigTreeValue,
} from "./config-tree";

const everything = "全部设置";
type Leaf = Extract<ConfigTreeNode, { value: unknown }>;
const leaf = (node: ConfigTreeNode): node is Leaf => "value" in node;

/** The label column of a tree row, then whatever stands for its value. */
function Row({ path, children }: { path: Path; children: React.ReactNode }) {
  const last = path.at(-1);
  return (
    <div className="setting-row tree-scalar">
      <span title={path.join(".")}>
        {typeof last === "number" ? `第 ${last + 1} 项` : last}
      </span>
      <div className="setting-value">{children}</div>
    </div>
  );
}

function Remove({ path, editing }: { path: Path; editing: Editing }) {
  return (
    <button
      className="icon-button"
      aria-label={`删除 ${path.join(".")}`}
      title="删除（恢复默认）"
      onClick={() => editing.change(path, undefined)}
    >
      <Trash2 size={14} />
    </button>
  );
}

/** Appends to a list of scalars, in the type the list already holds. */
function AddItem({
  path,
  items,
  editing,
}: {
  path: Path;
  items: Leaf[];
  editing: Editing;
}) {
  const [draft, setDraft] = useState("");
  const numeric = typeof items[0]?.value === "number";
  const name = path.join(".");
  const valid = draft.trim() !== "" && (!numeric || Number.isFinite(+draft));
  return (
    <form
      className="settings-list-row"
      onSubmit={(e) => {
        e.preventDefault();
        if (!valid) return;
        editing.change(
          [...path, items.length],
          numeric ? Number(draft) : draft.trim(),
        );
        setDraft("");
      }}
    >
      <input
        aria-label={`添加 ${name}`}
        type={numeric ? "number" : "text"}
        step="any"
        placeholder="添加一项"
        autoComplete="off"
        spellCheck={false}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
      />
      <button
        className="icon-button"
        aria-label={`确认添加 ${name}`}
        disabled={!valid}
      >
        <Plus size={15} />
      </button>
    </form>
  );
}

/** Adds one scalar key to a table. */
function AddField({
  path,
  taken,
  editing,
}: {
  path: Path;
  taken: string[];
  editing: Editing;
}) {
  const [adding, setAdding] = useState(false),
    [key, setKey] = useState(""),
    [type, setType] = useState("string"),
    [draft, setDraft] = useState("");
  const name = path.join(".") || "配置";
  if (!adding)
    return (
      <button
        type="button"
        className="text-button add-setting"
        aria-label={`${name} 添加字段`}
        onClick={() => setAdding(true)}
      >
        <Plus size={14} />
        添加字段
      </button>
    );
  const credential = credentialKey(key);
  const valid =
    key.trim() !== "" &&
    !credential &&
    !taken.includes(key.trim()) &&
    (type !== "number" || (draft.trim() !== "" && Number.isFinite(+draft)));
  return (
    <form
      className="settings-list-row"
      onSubmit={(e) => {
        e.preventDefault();
        if (!valid) return;
        editing.change(
          [...path, key.trim()],
          type === "boolean" ? false : type === "number" ? Number(draft) : draft,
        );
        setKey("");
        setDraft("");
        setAdding(false);
      }}
    >
      <input
        aria-label={`${name} 字段名称`}
        placeholder="字段名称"
        autoComplete="off"
        spellCheck={false}
        value={key}
        onChange={(e) => setKey(e.target.value)}
      />
      <select
        aria-label={`${name} 字段类型`}
        value={type}
        onChange={(e) => setType(e.target.value)}
      >
        <option value="string">文本</option>
        <option value="number">数字</option>
        <option value="boolean">开关</option>
      </select>
      {type !== "boolean" && (
        <input
          aria-label={`${name} 字段值`}
          type={type === "number" ? "number" : "text"}
          step="any"
          placeholder="值"
          autoComplete="off"
          spellCheck={false}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
        />
      )}
      {credential && <span className="config-readonly">凭据在原文中编辑</span>}
      <button className="secondary" disabled={!valid}>
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

/** The entries of a table or the items of a list. */
function Children({
  node,
  path,
  editing,
  hidden,
}: {
  node: Extract<ConfigTreeNode, { kind: "table" | "array" }>;
  path: Path;
  editing: Editing;
  /** Keys of this table that are shown elsewhere. */
  hidden?: string[];
}) {
  if (node.kind === "table")
    return (
      <>
        {node.entries.map(
          ([key, child]) =>
            !hidden?.includes(key) && (
              <TreeNode
                key={key}
                node={child}
                path={[...path, key]}
                editing={editing}
              />
            ),
        )}
        {/* Rust patches nothing below a credential key. */}
        {!path.some((seg) => credentialKey(String(seg))) && (
          <AddField
            path={path}
            taken={node.entries.map(([key]) => key)}
            editing={editing}
          />
        )}
      </>
    );
  return (
    <>
      {node.items.map((child, i) => (
        <TreeNode
          key={i}
          node={child}
          path={[...path, i]}
          editing={editing}
          item
        />
      ))}
      {node.items.every(leaf) && (
        <AddItem path={path} items={node.items} editing={editing} />
      )}
    </>
  );
}

const secretRow = (
  <>
    <span className="config-readonly">
      <code>••••••••</code>已设置
    </span>
    {sourceOnly}
  </>
);

function TreeNode({
  node,
  path,
  editing,
  item,
}: {
  node: ConfigTreeNode;
  path: Path;
  editing: Editing;
  /** Inside a list, where tables and nested lists stay in the source. */
  item?: boolean;
}) {
  if (node.kind === "secret") return <Row path={path}>{secretRow}</Row>;
  if (node.kind === "other")
    return (
      <Row path={path}>
        <code className="config-tree-text" title={node.text}>
          {node.text}
        </code>
        {sourceOnly}
      </Row>
    );
  if (leaf(node))
    return (
      <Row path={path}>
        <Scalar
          value={node.value}
          path={path}
          editing={editing}
          label={path.join(".")}
        />
        <Remove path={path} editing={editing} />
      </Row>
    );
  if (item) return <Row path={path}>{sourceOnly}</Row>;
  const last = path.at(-1);
  return (
    <details className="json-node">
      <summary>
        <span>{last}</span>
        <span className="node-count">
          {node.kind === "table" ? node.entries.length : node.items.length}
        </span>
      </summary>
      <div className="json-children">
        <Children node={node} path={path} editing={editing} />
      </div>
    </details>
  );
}

/** Starts a named sub-table with its first key. */
function AddTable({
  spec,
  taken,
  editing,
}: {
  spec: NamedTables;
  taken: string[];
  editing: Editing;
}) {
  const [name, setName] = useState(""),
    [key, setKey] = useState(spec.first[0]),
    [draft, setDraft] = useState("");
  const label = (k: string) =>
    spec.fields.find((f) => f.path.length === 1 && f.path[0] === k)?.label ?? k;
  const valid =
    name.trim() !== "" &&
    draft.trim() !== "" &&
    !credentialKey(name) &&
    !taken.includes(name.trim());
  return (
    <form
      className="settings-list-row"
      onSubmit={(e) => {
        e.preventDefault();
        if (!valid) return;
        editing.change([...spec.path, name.trim(), key], draft.trim());
        setName("");
        setDraft("");
      }}
    >
      <input
        aria-label={`新${spec.label}名称`}
        placeholder="名称"
        autoComplete="off"
        spellCheck={false}
        value={name}
        onChange={(e) => setName(e.target.value)}
      />
      {spec.first.length > 1 && (
        <select
          aria-label={`新${spec.label}首个字段`}
          value={key}
          onChange={(e) => setKey(e.target.value)}
        >
          {spec.first.map((k) => (
            <option key={k} value={k}>
              {label(k)}
            </option>
          ))}
        </select>
      )}
      <input
        aria-label={`新${spec.label}${label(key)}`}
        placeholder={label(key)}
        autoComplete="off"
        spellCheck={false}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
      />
      <button className="secondary" disabled={!valid}>
        添加
      </button>
    </form>
  );
}

/** One card per named sub-table: the template fields, then whatever else it holds. */
function Tables({
  spec,
  tree,
  editing,
}: {
  spec: NamedTables;
  tree: ConfigTreeNode;
  editing: Editing;
}) {
  const holder = nodeAt(tree, spec.path);
  if (holder && holder.kind !== "table")
    return <Row path={spec.path}>{sourceOnly}</Row>;
  const entries = holder?.entries ?? [];
  const known = [
    ...spec.fields.map((f) => String(f.path[0])),
    ...spec.source.map(([, key]) => key),
  ];
  return (
    <>
      {entries.length === 0 && <p className="muted">还没有{spec.label}</p>}
      {entries.map(([name, node]) => {
        const path = [...spec.path, name];
        if (node.kind !== "table")
          return (
            <Row key={name} path={path}>
              {node.kind === "secret" ? secretRow : sourceOnly}
            </Row>
          );
        return (
          <details className="json-node" key={name}>
            <summary>
              <span>{name}</span>
              <span className="node-count">{node.entries.length}</span>
            </summary>
            <div className="json-children">
              {spec.fields.map((field) => (
                <Setting
                  key={field.path.join(".")}
                  field={{
                    ...field,
                    path: [...path, ...field.path],
                    section: name,
                  }}
                  editing={editing}
                />
              ))}
              {spec.source.map(([label, key]) => (
                <div className="setting-row" key={key}>
                  <label>
                    {label}
                    <small>{key}</small>
                  </label>
                  <div className="setting-value">
                    {node.entries.some(([k]) => k === key) ? (
                      secretRow
                    ) : (
                      <>
                        <span className="config-readonly">未设置</span>
                        {sourceOnly}
                      </>
                    )}
                  </div>
                </div>
              ))}
              <Children
                node={node}
                path={path}
                editing={editing}
                hidden={known}
              />
            </div>
          </details>
        );
      })}
      <AddTable
        spec={spec}
        taken={entries.map(([name]) => name)}
        editing={editing}
      />
    </>
  );
}

/** The visual form over a TOML or YAML draft; Rust reads and patches the text. */
export default function ConfigTreeSettings({
  format,
  text,
  onChange,
  fields,
  tables = [],
  guarded = false,
}: {
  format: ConfigTreeFormat;
  text: string;
  onChange: (text: string) => void;
  /** Curated settings: group name → fields. */
  fields: Record<string, Field[]>;
  /** Tabs that list the named sub-tables of one table. */
  tables?: NamedTables[];
  guarded?: boolean;
}) {
  const groups = [
    ...Object.keys(fields),
    ...tables.map((spec) => spec.label),
    everything,
  ];
  const [group, setGroup] = useState(groups[0]),
    [tree, setTree] = useState<ConfigTreeNode | null>(null),
    [unreadable, setUnreadable] = useState(""),
    [error, setError] = useState("");
  const shown = groups.includes(group) ? group : groups[0];
  // Patches run one after another, each on the text the one before it produced.
  const latest = useRef(text),
    seen = useRef(text),
    queue = useRef(Promise.resolve()),
    changed = useRef(onChange);
  changed.current = onChange;
  if (seen.current !== text) seen.current = latest.current = text;
  useEffect(() => {
    let stale = false;
    command("read_config_tree", { format, text }).then(
      (next) => {
        if (stale) return;
        setTree(next);
        setUnreadable("");
      },
      // The last tree that was read stays on screen.
      (e) => stale || setUnreadable(errorOf(e).message),
    );
    return () => {
      stale = true;
    };
  }, [format, text]);
  const editing: Editing = {
    guarded,
    get: (path) => {
      const node = tree && nodeAt(tree, path);
      return node ? plain(node) : undefined;
    },
    secret: (path) => (tree && nodeAt(tree, path))?.kind === "secret",
    change: (path, value) => {
      queue.current = queue.current.then(async () => {
        try {
          const next = await command("patch_config_tree", {
            format,
            text: latest.current,
            path,
            value:
              value === undefined ? null : (value as ConfigTreeValue | null),
          });
          latest.current = next;
          setError("");
          changed.current(next);
        } catch (e) {
          setError(errorOf(e).message);
        }
      });
    },
  };
  return (
    <div className="claude-settings config-tree-settings">
      {groups.length > 1 && (
        <nav className="settings-tabs" aria-label="配置分组">
          {groups.map((g) => (
            <button
              key={g}
              aria-pressed={shown === g}
              onClick={() => setGroup(g)}
            >
              {g}
            </button>
          ))}
        </nav>
      )}
      <div className="settings-content">
        {unreadable && (
          <div className="inline-error" role="alert">
            {unreadable}；请切换到原文模式修正
          </div>
        )}
        {error && (
          <div className="inline-error" role="alert">
            {error}
          </div>
        )}
        {!tree ? (
          !unreadable && <div className="empty">正在读取配置结构…</div>
        ) : fields[shown] ? (
          <Settings fields={fields[shown]} editing={editing} />
        ) : shown !== everything ? (
          <Tables
            spec={tables.find((spec) => spec.label === shown)!}
            tree={tree}
            editing={editing}
          />
        ) : tree.kind !== "table" && tree.kind !== "array" ? (
          <Row path={["配置"]}>{sourceOnly}</Row>
        ) : tree.kind === "table" || tree.items.length ? (
          <Children node={tree} path={[]} editing={editing} />
        ) : (
          <p className="muted">文件中还没有设置</p>
        )}
      </div>
    </div>
  );
}
