import { useEffect, useRef, useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { command } from "./bridge";
import { errorOf } from "./types";
import type { Field, Path } from "./claude-settings";
import { Scalar, Settings, sourceOnly, type Editing } from "./ClaudeSettings";
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

/** The entries of a table or the items of a list. */
function Children({
  node,
  path,
  editing,
}: {
  node: Extract<ConfigTreeNode, { kind: "table" | "array" }>;
  path: Path;
  editing: Editing;
}) {
  if (node.kind === "table")
    return node.entries.map(([key, child]) => (
      <TreeNode
        key={key}
        node={child}
        path={[...path, key]}
        editing={editing}
      />
    ));
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
  if (node.kind === "secret")
    return (
      <Row path={path}>
        <span className="config-readonly">
          <code>••••••••</code>已设置
        </span>
      </Row>
    );
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

/** The visual form over a TOML or YAML draft; Rust reads and patches the text. */
export default function ConfigTreeSettings({
  format,
  text,
  onChange,
  fields,
  guarded = false,
}: {
  format: ConfigTreeFormat;
  text: string;
  onChange: (text: string) => void;
  /** Curated settings: group name → fields. */
  fields: Record<string, Field[]>;
  guarded?: boolean;
}) {
  const groups = [...Object.keys(fields), everything];
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
        ) : shown !== everything ? (
          <Settings fields={fields[shown]} editing={editing} />
        ) : tree.kind !== "table" && tree.kind !== "array" ? (
          <Row path={["配置"]}>{sourceOnly}</Row>
        ) : (tree.kind === "table" ? tree.entries : tree.items).length ? (
          <Children node={tree} path={[]} editing={editing} />
        ) : (
          <p className="muted">文件中还没有设置</p>
        )}
      </div>
    </div>
  );
}
