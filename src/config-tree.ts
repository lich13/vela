import type { Json, Path } from "./claude-settings";

export type ConfigTreeFormat = "toml" | "yaml";
/** The structure Rust reads from a TOML or YAML draft. */
export type ConfigTreeNode =
  | { kind: "table"; entries: [string, ConfigTreeNode][] }
  | { kind: "array"; items: ConfigTreeNode[] }
  | { kind: "string"; value: string }
  | { kind: "integer" | "float"; value: number }
  | { kind: "boolean"; value: boolean }
  /** Shown, never edited here: dates, null, multi-line text, YAML tags and aliases. */
  | { kind: "other"; text: string }
  /** A credential: Rust reports only that it exists. */
  | { kind: "secret" };
/** What a visual control may write; `null` removes the key or item. */
export type ConfigTreeValue = boolean | number | string | (boolean | number | string)[];

export function nodeAt(
  root: ConfigTreeNode,
  path: Path,
): ConfigTreeNode | undefined {
  let node: ConfigTreeNode | undefined = root;
  for (const seg of path) {
    if (node?.kind === "table" && typeof seg === "string")
      node = node.entries.find(([key]) => key === seg)?.[1];
    else if (node?.kind === "array" && typeof seg === "number")
      node = node.items[seg];
    else return undefined;
  }
  return node;
}

/** Stands for a value the form cannot show or edit. */
const opaque: Json = Object.freeze({});

export function plain(node: ConfigTreeNode): Json {
  switch (node.kind) {
    case "table":
      return Object.fromEntries(node.entries.map(([k, v]) => [k, plain(v)]));
    case "array":
      return node.items.map(plain);
    case "other":
    case "secret":
      return opaque;
    default:
      return node.value;
  }
}
