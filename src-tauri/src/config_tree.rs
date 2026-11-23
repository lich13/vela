//! Structure view and minimal text patches for TOML and YAML drafts.
//!
//! The visual editors never rewrite a document. Each edit replaces, inserts or
//! removes one byte range of the draft; the result is parsed again and must
//! differ from the original by exactly the requested change, otherwise the
//! draft stays untouched and the caller gets an error.
use crate::errors::code;
use crate::storage::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::str::FromStr;
use toml_edit::{Array, ArrayOfTables, InlineTable, Item, Table, Value};
use yaml_edit::{AsYaml, Mapping, Scalar, Sequence, SyntaxKind, YamlFile, YamlNode};

const LIMIT: usize = 2 * 1024 * 1024;
const DEPTH: usize = 64;
// Larger integers lose precision in the web view.
const SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Toml,
    Yaml,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Seg {
    Index(usize),
    Key(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Node {
    Table {
        entries: Vec<(String, Node)>,
    },
    Array {
        items: Vec<Node>,
    },
    String {
        value: String,
    },
    Integer {
        value: i64,
    },
    Float {
        value: f64,
    },
    Boolean {
        value: bool,
    },
    /// Shown but never edited here: dates, null, multi-line text, YAML tags and aliases.
    Other {
        text: String,
    },
    /// A value under a credential key: only its presence is reported.
    Secret,
}

fn unsupported() -> AppError {
    AppError::new(
        code::CONFIG,
        "这个字段的结构无法在可视化中修改，请在原文中编辑",
    )
}
fn syntax(format: Format) -> AppError {
    match format {
        Format::Toml => AppError::new(code::TOML, "TOML 语法错误，请先在原文中修正"),
        Format::Yaml => AppError::new(code::CONFIG, "YAML 格式无效，请先在原文中修正"),
    }
}
fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}
fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}
/// The offset just past the line break that follows `at`.
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i + 1)
}
fn text_node(value: &str) -> Node {
    if value.contains(['\n', '\r']) {
        Node::Other { text: value.into() }
    } else {
        Node::String {
            value: value.into(),
        }
    }
}
fn integer_node(value: i64, raw: &str) -> Node {
    if value.abs() > SAFE_INTEGER {
        Node::Other { text: raw.into() }
    } else {
        Node::Integer { value }
    }
}

/// The value a visual control may write: a scalar or a list of scalars.
fn requested(value: &serde_json::Value, nested: bool) -> Result<Node> {
    Ok(match value {
        serde_json::Value::Bool(value) => Node::Boolean { value: *value },
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(value) if value.abs() <= SAFE_INTEGER => Node::Integer { value },
            Some(_) => return Err(unsupported()),
            None => Node::Float {
                value: n
                    .as_f64()
                    .filter(|f| f.is_finite())
                    .ok_or_else(unsupported)?,
            },
        },
        serde_json::Value::String(value) if !value.contains(['\n', '\r']) => Node::String {
            value: value.clone(),
        },
        serde_json::Value::Array(items) if !nested => Node::Array {
            items: items
                .iter()
                .map(|item| requested(item, true))
                .collect::<Result<_>>()?,
        },
        _ => return Err(unsupported()),
    })
}

/// Apply the requested change to the parsed structure; this is what the patched text must equal.
fn apply(node: &mut Node, path: &[Seg], value: Option<Node>) -> Result<()> {
    let Some((seg, rest)) = path.split_first() else {
        return Err(unsupported());
    };
    match (node, seg) {
        (Node::Table { entries }, Seg::Key(key)) => {
            let at = entries.iter().position(|(k, _)| k == key);
            if rest.is_empty() {
                match (at, value) {
                    (Some(i), Some(value)) => entries[i].1 = value,
                    (Some(i), None) => {
                        entries.remove(i);
                    }
                    (None, Some(value)) => entries.push((key.clone(), value)),
                    (None, None) => {}
                }
                return Ok(());
            }
            match at {
                Some(i) => apply(&mut entries[i].1, rest, value),
                None if value.is_some() => {
                    let mut child = Node::Table { entries: vec![] };
                    apply(&mut child, rest, value)?;
                    entries.push((key.clone(), child));
                    Ok(())
                }
                None => Ok(()),
            }
        }
        (Node::Array { items }, Seg::Index(index)) => {
            if rest.is_empty() {
                match value {
                    Some(value) if *index < items.len() => items[*index] = value,
                    Some(value) if *index == items.len() => items.push(value),
                    Some(_) => return Err(unsupported()),
                    None if *index < items.len() => {
                        items.remove(*index);
                    }
                    None => {}
                }
                return Ok(());
            }
            let end = items.len();
            match items.get_mut(*index) {
                Some(child) => apply(child, rest, value),
                None if value.is_none() => Ok(()),
                // A new row starts as a table that holds the requested key.
                None if *index == end => {
                    let mut child = Node::Table { entries: vec![] };
                    apply(&mut child, rest, value)?;
                    items.push(child);
                    Ok(())
                }
                None => Err(unsupported()),
            }
        }
        _ => Err(unsupported()),
    }
}

/// Key order is not part of the comparison, and TOML cannot tell an empty table from a missing one.
fn canonical(node: &mut Node, prune: bool) {
    match node {
        Node::Table { entries } => {
            for (_, child) in entries.iter_mut() {
                canonical(child, prune);
            }
            if prune {
                entries.retain(
                    |(_, child)| !matches!(child, Node::Table { entries } if entries.is_empty()),
                );
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Node::Array { items } => items.iter_mut().for_each(|child| canonical(child, prune)),
        _ => {}
    }
}

pub fn tree(format: Format, text: &str) -> Result<Node> {
    if text.len() > LIMIT {
        return Err(AppError::new(code::CONFIG, "配置文件超过 2 MiB"));
    }
    match format {
        Format::Toml => {
            let doc = toml_edit::Document::parse(text).map_err(|_| syntax(format))?;
            Ok(toml_table(doc.as_table(), 0))
        }
        Format::Yaml => Ok(yaml_node(&yaml_root(text)?, 0)),
    }
}

/// Whether a key names a credential. Words that ordinary settings share
/// (`token_limit`, `credentials_store`, `apiKeyEnv`) count only at the end of the key.
fn sensitive(key: &str) -> bool {
    let mut words = vec![String::new()];
    let mut lower = false;
    for c in key.chars() {
        if !c.is_ascii_alphanumeric() || (lower && c.is_ascii_uppercase()) {
            words.push(String::new());
        }
        if c.is_ascii_alphanumeric() {
            let last = words.len() - 1;
            words[last].push(c.to_ascii_lowercase());
        }
        lower = c.is_ascii_lowercase() || c.is_ascii_digit();
    }
    words.retain(|word| !word.is_empty());
    let last = words.last().map_or("", String::as_str);
    const ANYWHERE: &[&str] = &[
        "secret",
        "secrets",
        "password",
        "passwords",
        "passwd",
        "passphrase",
        "authorization",
        "bearer",
        "cookie",
        "cookies",
    ];
    const LAST: &[&str] = &[
        "token",
        "credential",
        "credentials",
        "apikey",
        "header",
        "headers",
    ];
    words.iter().any(|word| ANYWHERE.contains(&word.as_str()))
        || LAST.contains(&last)
        || (matches!(last, "key" | "keys") && words.len() > 1)
}

/// Hide every value below a credential key.
fn mask(node: &mut Node, hidden: bool) {
    match node {
        Node::Table { entries } => {
            for (key, child) in entries.iter_mut() {
                mask(child, hidden || sensitive(key));
            }
        }
        Node::Array { items } => items.iter_mut().for_each(|child| mask(child, hidden)),
        scalar if hidden => *scalar = Node::Secret,
        _ => {}
    }
}

pub fn patch(
    format: Format,
    text: &str,
    path: &[Seg],
    value: Option<&serde_json::Value>,
) -> Result<String> {
    // Credentials are edited in the source only, where nothing has to echo them back.
    if path
        .iter()
        .any(|seg| matches!(seg, Seg::Key(key) if sensitive(key)))
    {
        return Err(unsupported());
    }
    let value = value.map(|v| requested(v, false)).transpose()?;
    let mut expected = tree(format, text)?;
    let mut after = match format {
        Format::Toml => toml_patch(text, path, value)?,
        Format::Yaml => yaml_patch(text, path, value)?,
    };
    // Only the surgery knows which numeric type it kept, so it reports the value it wrote.
    apply(&mut expected, path, after.1.take())?;
    let mut actual = tree(format, &after.0).map_err(|_| unsupported())?;
    let prune = format == Format::Toml;
    canonical(&mut expected, prune);
    canonical(&mut actual, prune);
    if expected != actual {
        return Err(unsupported());
    }
    Ok(after.0)
}

#[tauri::command]
pub fn read_config_tree(format: Format, text: String) -> Result<Node> {
    let mut node = tree(format, &text)?;
    mask(&mut node, false);
    Ok(node)
}

#[tauri::command]
pub fn patch_config_tree(
    format: Format,
    text: String,
    path: Vec<Seg>,
    value: Option<serde_json::Value>,
) -> Result<String> {
    patch(format, &text, &path, value.as_ref())
}

// ---------------------------------------------------------------- TOML

fn toml_table(table: &Table, depth: usize) -> Node {
    if depth > DEPTH {
        return Node::Other {
            text: "嵌套过深".into(),
        };
    }
    Node::Table {
        entries: table
            .iter()
            .filter_map(|(key, item)| Some((key.to_owned(), toml_item(item, depth + 1)?)))
            .collect(),
    }
}
fn toml_item(item: &Item, depth: usize) -> Option<Node> {
    Some(match item {
        Item::None => return None,
        Item::Value(value) => toml_value(value, depth),
        Item::Table(table) => toml_table(table, depth),
        Item::ArrayOfTables(tables) => Node::Array {
            items: tables.iter().map(|t| toml_table(t, depth + 1)).collect(),
        },
    })
}
fn toml_value(value: &Value, depth: usize) -> Node {
    if depth > DEPTH {
        return Node::Other {
            text: "嵌套过深".into(),
        };
    }
    match value {
        Value::String(s) => text_node(s.value()),
        Value::Integer(i) => integer_node(*i.value(), &i.value().to_string()),
        Value::Float(f) if f.value().is_finite() => Node::Float { value: *f.value() },
        Value::Float(f) => Node::Other {
            text: f.value().to_string(),
        },
        Value::Boolean(b) => Node::Boolean { value: *b.value() },
        Value::Datetime(d) => Node::Other {
            text: d.value().to_string(),
        },
        Value::Array(array) => Node::Array {
            items: array.iter().map(|v| toml_value(v, depth + 1)).collect(),
        },
        Value::InlineTable(table) => Node::Table {
            entries: table
                .iter()
                .map(|(key, v)| (key.to_owned(), toml_value(v, depth + 1)))
                .collect(),
        },
    }
}

#[derive(Clone, Copy)]
enum At<'a> {
    Table(&'a Table),
    Inline(&'a InlineTable),
    Array(&'a Array),
    Tables(&'a ArrayOfTables),
    Scalar(&'a Value),
}
impl<'a> At<'a> {
    fn item(item: &'a Item) -> Option<Self> {
        match item {
            Item::None => None,
            Item::Value(value) => Some(Self::value(value)),
            Item::Table(table) => Some(Self::Table(table)),
            Item::ArrayOfTables(tables) => Some(Self::Tables(tables)),
        }
    }
    fn value(value: &'a Value) -> Self {
        match value {
            Value::InlineTable(table) => Self::Inline(table),
            Value::Array(array) => Self::Array(array),
            scalar => Self::Scalar(scalar),
        }
    }
    fn child(self, seg: &Seg) -> Option<Self> {
        match (self, seg) {
            (Self::Table(table), Seg::Key(key)) => table.get(key).and_then(Self::item),
            (Self::Inline(table), Seg::Key(key)) => table.get(key).map(Self::value),
            (Self::Array(array), Seg::Index(index)) => array.get(*index).map(Self::value),
            (Self::Tables(tables), Seg::Index(index)) => tables.get(*index).map(Self::Table),
            _ => None,
        }
    }
    /// The span of an inline value; tables written with headers have none to replace.
    fn span(self) -> Option<Range<usize>> {
        match self {
            Self::Inline(table) => table.span(),
            Self::Array(array) => array.span(),
            Self::Scalar(value) => value.span(),
            Self::Table(_) | Self::Tables(_) => None,
        }
    }
}

fn toml_keys(path: &[Seg]) -> Result<String> {
    let keys = path
        .iter()
        .map(|seg| match seg {
            Seg::Key(key) => Ok(toml_edit::Key::new(key).to_string()),
            Seg::Index(_) => Err(unsupported()),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(keys.join("."))
}
fn toml_text(value: &Node, old: Option<&str>) -> Result<String> {
    Ok(match value {
        Node::Boolean { value } => value.to_string(),
        Node::Integer { value } => value.to_string(),
        Node::Float { value } => Value::from(*value).to_string(),
        Node::String { value } => {
            // A literal string stays literal when the new text allows it.
            let literal = old.is_some_and(|raw| raw.starts_with('\'') && !raw.starts_with("'''"))
                && !value
                    .chars()
                    .any(|c| c == '\'' || (c.is_control() && c != '\t'));
            if literal {
                format!("'{value}'")
            } else {
                Value::from(value.as_str()).to_string()
            }
        }
        Node::Array { items } => format!(
            "[{}]",
            items
                .iter()
                .map(|item| toml_text(item, None))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        _ => return Err(unsupported()),
    })
}
/// The whole line when `span` is the only item on it, so its own comma and comment go with it.
fn own_line(text: &str, span: &Range<usize>) -> Option<Range<usize>> {
    let start = line_start(text, span.start);
    let end = line_end(text, span.end);
    let after = text[span.end..end].trim();
    let after = after.strip_prefix(',').unwrap_or(after).trim_start();
    (text[start..span.start].trim().is_empty() && (after.is_empty() || after.starts_with('#')))
        .then_some(start..end)
}
/// Remove one comma-separated entry together with exactly one separator.
fn remove_listed(
    out: &mut String,
    text: &str,
    entries: &[Range<usize>],
    index: usize,
) -> Result<()> {
    let entry = entries.get(index).ok_or_else(unsupported)?;
    if let Some(next) = entries.get(index + 1) {
        out.replace_range(entry.start..next.start, "");
    } else if index > 0 {
        let previous = entries[index - 1].end;
        if text[previous..entry.start].contains('\n') {
            // The previous line keeps its comma and comment; only this entry's part of its own
            // line goes, with the trailing comma that would otherwise follow the kept one.
            let blank = |rest: &str| rest.len() - rest.trim_start_matches([' ', '\t']).len();
            let mut end = entry.end + blank(&text[entry.end..]);
            if text[end..].starts_with(',') {
                end += 1 + blank(&text[end + 1..]);
            }
            out.replace_range(line_start(text, entry.start)..end, "");
        } else {
            out.replace_range(previous..entry.end, "");
        }
    } else {
        out.replace_range(entry.clone(), "");
    }
    Ok(())
}
fn values_end(table: &Table, last: &mut Option<usize>) {
    for (_, item) in table.iter() {
        match item {
            Item::Value(value) => {
                if let Some(span) = value.span() {
                    *last = Some(last.map_or(span.end, |end| end.max(span.end)));
                }
            }
            Item::Table(inner) if inner.is_dotted() => values_end(inner, last),
            _ => {}
        }
    }
}
fn first_header(table: &Table, first: &mut Option<usize>) {
    let mut see = |span: Option<Range<usize>>| {
        if let Some(span) = span {
            *first = Some(first.map_or(span.start, |start| start.min(span.start)));
        }
    };
    for (_, item) in table.iter() {
        match item {
            Item::Table(inner) if !inner.is_dotted() => see(inner.span()),
            Item::ArrayOfTables(tables) => tables.iter().for_each(|t| see(t.span())),
            _ => {}
        }
    }
    for (_, item) in table.iter() {
        match item {
            Item::Table(inner) if !inner.is_dotted() => first_header(inner, first),
            Item::ArrayOfTables(tables) => tables.iter().for_each(|t| first_header(t, first)),
            _ => {}
        }
    }
}
/// Where a new `key = value` line of `table` goes, and whether a blank line must follow it.
fn body_end(text: &str, table: &Table, root: Option<&Table>) -> Result<(usize, bool)> {
    let mut last = None;
    values_end(table, &mut last);
    if let Some(end) = last {
        return Ok((line_end(text, end), false));
    }
    let Some(root) = root else {
        let header = table.span().ok_or_else(unsupported)?;
        return Ok((line_end(text, header.end), false));
    };
    // Root keys must precede every table header; comments directly above a header belong to it.
    let mut first = None;
    first_header(root, &mut first);
    let Some(header) = first else {
        return Ok((text.len(), false));
    };
    let mut at = line_start(text, header);
    while at > 0 {
        let previous = line_start(text, at - 1);
        if !text[previous..at].trim_start().starts_with('#') {
            break;
        }
        at = previous;
    }
    Ok((at, true))
}
fn insert_line(out: &mut String, text: &str, at: usize, line: &str, gap: bool) {
    let nl = newline(text);
    let lead = if at > 0 && !text[..at].ends_with('\n') {
        nl
    } else {
        ""
    };
    out.insert_str(
        at,
        &format!("{lead}{line}{nl}{}", if gap { nl } else { "" }),
    );
}
fn inline_insert(out: &mut String, table: &InlineTable, entry: &str) -> Result<()> {
    if table.is_dotted() {
        return Err(unsupported());
    }
    let last = table
        .iter()
        .filter_map(|(_, v)| v.span())
        .map(|s| s.end)
        .max();
    match last {
        Some(end) => out.insert_str(end, &format!(", {entry}")),
        None => {
            let span = table.span().ok_or_else(unsupported)?;
            out.replace_range(span, &format!("{{ {entry} }}"));
        }
    }
    Ok(())
}
fn array_append(out: &mut String, text: &str, array: &Array, item: &str) -> Result<()> {
    let span = array.span().ok_or_else(unsupported)?;
    let Some(last) = array.iter().last().and_then(|v| v.span()) else {
        out.replace_range(span, &format!("[{item}]"));
        return Ok(());
    };
    let close = span.end - 1;
    let tail = &text[last.end..close];
    if !tail.contains('\n') {
        out.insert_str(last.end, &format!(", {item}"));
        return Ok(());
    }
    // One item per line: add a line before the closing bracket and keep the comma style.
    let comma = tail.trim_start().starts_with(',');
    let start = line_start(text, last.start);
    let indent = if text[start..last.start].trim().is_empty() {
        &text[start..last.start]
    } else {
        "  "
    };
    let nl = newline(text);
    out.insert_str(
        line_start(text, close),
        &format!("{indent}{item}{}{nl}", if comma { "," } else { "" }),
    );
    if !comma {
        out.insert(last.end, ',');
    }
    Ok(())
}

/// Returns the patched text and the value actually written (a float key keeps its type).
fn toml_patch(text: &str, path: &[Seg], value: Option<Node>) -> Result<(String, Option<Node>)> {
    let doc = toml_edit::Document::parse(text).map_err(|_| syntax(Format::Toml))?;
    let root = doc.as_table();
    let mut chain = vec![At::Table(root)];
    for seg in path {
        match chain[chain.len() - 1].child(seg) {
            Some(next) => chain.push(next),
            None => break,
        }
    }
    let found = chain.len() - 1;
    let mut out = text.to_owned();
    if path.is_empty() {
        return Err(unsupported());
    }
    if found == path.len() {
        let target = chain[found];
        let parent = chain[found - 1];
        let Some(value) = value else {
            match (parent, &path[found - 1]) {
                (At::Table(table), Seg::Key(key)) => {
                    // Only `key = value` lines are removed here; whole tables need the source editor.
                    let span = target.span().ok_or_else(unsupported)?;
                    let key = table
                        .key(key)
                        .and_then(|k| k.span())
                        .ok_or_else(unsupported)?;
                    out.replace_range(line_start(text, key.start)..line_end(text, span.end), "");
                }
                (At::Inline(table), Seg::Key(key)) => {
                    let entries: Vec<_> = table
                        .iter()
                        .filter_map(|(k, v)| Some(table.key(k)?.span()?.start..v.span()?.end))
                        .collect();
                    let index = table
                        .iter()
                        .position(|(k, _)| k == key)
                        .ok_or_else(unsupported)?;
                    if entries.len() != table.len() {
                        return Err(unsupported());
                    }
                    remove_listed(&mut out, text, &entries, index)?;
                }
                (At::Array(array), Seg::Index(index)) => {
                    let entries: Vec<_> = array.iter().filter_map(|v| v.span()).collect();
                    if entries.len() != array.len() {
                        return Err(unsupported());
                    }
                    match own_line(text, &entries[*index]) {
                        Some(line) => out.replace_range(line, ""),
                        None => remove_listed(&mut out, text, &entries, *index)?,
                    }
                }
                _ => return Err(unsupported()),
            }
            return Ok((out, None));
        };
        let span = target.span().ok_or_else(unsupported)?;
        let value = match (value, target) {
            (Node::Integer { value }, At::Scalar(Value::Float(_))) => Node::Float {
                value: value as f64,
            },
            (value, _) => value,
        };
        out.replace_range(span.clone(), &toml_text(&value, Some(&text[span]))?);
        return Ok((out, Some(value)));
    }
    let Some(value) = value else {
        return Ok((out, None));
    };
    let rest = &path[found..];
    let encoded = toml_text(&value, None)?;
    match chain[found] {
        At::Array(array) => {
            if !matches!(rest, [Seg::Index(index)] if *index == array.len())
                || matches!(value, Node::Array { .. })
            {
                return Err(unsupported());
            }
            array_append(&mut out, text, array, &encoded)?;
        }
        At::Inline(table) => inline_insert(
            &mut out,
            table,
            &format!("{} = {encoded}", toml_keys(rest)?),
        )?,
        At::Table(table) => {
            let in_tables = chain[..=found].iter().any(|at| matches!(at, At::Tables(_)));
            if table.is_dotted() {
                // `a.b = …` keys live in the body of the nearest table that has its own header.
                let mut owner = found;
                while owner > 0 && matches!(chain[owner], At::Table(t) if t.is_dotted()) {
                    owner -= 1;
                }
                let entry = format!("{} = {encoded}", toml_keys(&path[owner..])?);
                match chain[owner] {
                    At::Table(holder) => {
                        let (at, gap) = body_end(text, holder, (owner == 0).then_some(root))?;
                        insert_line(&mut out, text, at, &entry, gap);
                    }
                    At::Inline(holder) => inline_insert(&mut out, holder, &entry)?,
                    _ => return Err(unsupported()),
                }
            } else if in_tables || (!table.is_implicit() && rest.len() == 1) {
                let (at, gap) = body_end(text, table, (found == 0).then_some(root))?;
                insert_line(
                    &mut out,
                    text,
                    at,
                    &format!("{} = {encoded}", toml_keys(rest)?),
                    gap,
                );
            } else {
                // A new `[table]` at the end of the file leaves every existing line as it is.
                let nl = newline(text);
                let body = text.trim_end_matches(['\n', '\r', ' ', '\t']);
                let breaks = text[body.len()..].matches('\n').count();
                if !body.is_empty() {
                    for _ in breaks..2 {
                        out.push_str(nl);
                    }
                }
                let (last, header) = path.split_last().ok_or_else(unsupported)?;
                out.push_str(&format!(
                    "[{}]{nl}{} = {encoded}{nl}",
                    toml_keys(header)?,
                    toml_keys(std::slice::from_ref(last))?
                ));
            }
        }
        At::Tables(_) | At::Scalar(_) => return Err(unsupported()),
    }
    Ok((out, Some(value)))
}

// ---------------------------------------------------------------- YAML

fn yaml_root(text: &str) -> Result<YamlNode> {
    let file = YamlFile::from_str(text).map_err(|_| syntax(Format::Yaml))?;
    if file.documents().count() != 1 {
        return Err(AppError::new(code::CONFIG, "YAML 必须是单个文档"));
    }
    let doc = file.document().ok_or_else(|| syntax(Format::Yaml))?;
    doc.as_sequence()
        .map(YamlNode::Sequence)
        .or_else(|| doc.as_mapping().map(YamlNode::Mapping))
        .ok_or_else(|| AppError::new(code::CONFIG, "可视化只支持 YAML 映射或数组"))
}
fn node_span(node: &impl AsYaml) -> Option<Range<usize>> {
    let range = node.as_node()?.text_range();
    Some(usize::from(range.start())..usize::from(range.end()))
}
fn child_spans(node: &impl AsYaml, kind: SyntaxKind) -> Vec<Range<usize>> {
    node.as_node()
        .map(|n| {
            n.children()
                .filter(|child| child.kind() == kind)
                .map(|child| {
                    let range = child.text_range();
                    usize::from(range.start())..usize::from(range.end())
                })
                .collect()
        })
        .unwrap_or_default()
}
/// The `key: value` or `- item` node that holds `node`.
fn entry_span(node: &YamlNode) -> Option<Range<usize>> {
    let mut current = node.as_node()?.clone();
    loop {
        let up = current.parent()?;
        match up.kind() {
            SyntaxKind::MAPPING_ENTRY | SyntaxKind::SEQUENCE_ENTRY => {
                let range = up.text_range();
                return Some(usize::from(range.start())..usize::from(range.end()));
            }
            SyntaxKind::VALUE => current = up,
            _ => return None,
        }
    }
}
/// The offset right after `key:` when `node` is the value of a mapping entry.
fn value_start(node: &impl AsYaml) -> Option<usize> {
    let up = node.as_node()?.parent()?;
    (up.kind() == SyntaxKind::VALUE).then(|| usize::from(up.text_range().start()))
}
/// Entries by key; `None` when a key is not a plain scalar, repeats, or an entry cannot be read.
fn yaml_entries(map: &Mapping) -> Option<Vec<(String, YamlNode)>> {
    let mut entries: Vec<(String, YamlNode)> = Vec::new();
    for (key, value) in map.iter() {
        let key = key.as_scalar()?.as_string();
        if entries.iter().any(|(known, _)| *known == key) {
            return None;
        }
        entries.push((key, value));
    }
    (entries.len() == child_spans(map, SyntaxKind::MAPPING_ENTRY).len()).then_some(entries)
}
fn yaml_items(seq: &Sequence) -> Option<Vec<YamlNode>> {
    let items: Vec<_> = seq.values().collect();
    (items.len() == child_spans(seq, SyntaxKind::SEQUENCE_ENTRY).len()).then_some(items)
}
fn yaml_scalar(scalar: &Scalar) -> Node {
    let raw = scalar.value();
    if raw.starts_with(['|', '>']) || raw.contains('\n') {
        return Node::Other {
            text: scalar.as_string(),
        };
    }
    if scalar.is_quoted() {
        return text_node(&scalar.as_string());
    }
    match raw.as_str() {
        "" | "~" | "null" | "Null" | "NULL" => {
            return Node::Other {
                text: "null".into(),
            }
        }
        "true" | "True" | "TRUE" => return Node::Boolean { value: true },
        "false" | "False" | "FALSE" => return Node::Boolean { value: false },
        _ => {}
    }
    let digits = raw.trim_start_matches(['+', '-']);
    let numeric = digits.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        && raw.len() - digits.len() <= 1;
    if numeric && digits.bytes().all(|b| b.is_ascii_digit()) {
        return match raw.parse::<i64>() {
            Ok(value) => integer_node(value, &raw),
            Err(_) => Node::Other { text: raw },
        };
    }
    if numeric
        && digits
            .bytes()
            .all(|b| b.is_ascii_digit() || b"._eE+-".contains(&b))
    {
        if let Some(value) = raw.parse::<f64>().ok().filter(|f| f.is_finite()) {
            return Node::Float { value };
        }
    }
    // Other YAML number forms (hex, octal, infinity) are not text and are left to the source editor.
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("0x")
        || lower.starts_with("0o")
        || matches!(lower.as_str(), ".inf" | "+.inf" | "-.inf" | ".nan")
    {
        return Node::Other { text: raw };
    }
    Node::String { value: raw }
}
fn yaml_node(node: &YamlNode, depth: usize) -> Node {
    // Unreadable parts are named, not quoted: their text may hold credentials under any key.
    let opaque = |text: &str| Node::Other { text: text.into() };
    if depth > DEPTH {
        return opaque("嵌套过深");
    }
    match node {
        YamlNode::Scalar(scalar) => yaml_scalar(scalar),
        YamlNode::Mapping(map) => match yaml_entries(map) {
            Some(entries) => Node::Table {
                entries: entries
                    .iter()
                    .map(|(key, value)| (key.clone(), yaml_node(value, depth + 1)))
                    .collect(),
            },
            None => opaque("含重复键或非文本键的映射"),
        },
        YamlNode::Sequence(seq) => match yaml_items(seq) {
            Some(items) => Node::Array {
                items: items
                    .iter()
                    .map(|item| yaml_node(item, depth + 1))
                    .collect(),
            },
            None => opaque("无法逐项读取的数组"),
        },
        YamlNode::Alias(_) => opaque("别名引用"),
        YamlNode::TaggedNode(_) => opaque("带标签的值"),
    }
}
fn yaml_child(node: &YamlNode, seg: &Seg) -> Option<YamlNode> {
    match (node, seg) {
        (YamlNode::Mapping(map), Seg::Key(key)) => yaml_entries(map)?
            .into_iter()
            .find(|(known, _)| known == key)
            .map(|(_, value)| value),
        (YamlNode::Sequence(seq), Seg::Index(index)) => yaml_items(seq)?.into_iter().nth(*index),
        _ => None,
    }
}

/// Text that every YAML schema reads back as the same plain string.
fn plain(value: &str) -> bool {
    let reserved = ["true", "false", "null", "yes", "no", "on", "off", "y", "n"];
    value
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '/')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./@+".contains(c))
        && !reserved.contains(&value.to_ascii_lowercase().as_str())
}
fn quoted(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(|_| unsupported())
}
fn yaml_text(value: &Node, old: Option<&str>) -> Result<String> {
    Ok(match value {
        Node::Boolean { value } => value.to_string(),
        Node::Integer { value } => value.to_string(),
        Node::Float { value } => value.to_string(),
        Node::String { value } => {
            let control = value.chars().any(char::is_control);
            match old.and_then(|raw| raw.chars().next()) {
                Some('"') => quoted(value)?,
                Some('\'') if !control => format!("'{}'", value.replace('\'', "''")),
                _ if plain(value) => value.clone(),
                _ => quoted(value)?,
            }
        }
        Node::Array { items } => format!(
            "[{}]",
            items
                .iter()
                .map(|item| yaml_text(item, None))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        Node::Table { entries } => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, item)| Ok(format!("{}: {}", yaml_key(key)?, yaml_text(item, None)?)))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        Node::Other { .. } | Node::Secret => return Err(unsupported()),
    })
}
fn yaml_key(key: &str) -> Result<String> {
    if plain(key) {
        Ok(key.into())
    } else {
        quoted(key)
    }
}
/// `key: value` at `indent`; nested collections continue in block style below it.
fn yaml_entry(out: &mut String, key: &str, value: &Node, indent: usize, nl: &str) -> Result<()> {
    let pad = " ".repeat(indent);
    let key = yaml_key(key)?;
    match value {
        Node::Table { entries } if !entries.is_empty() => {
            out.push_str(&format!("{pad}{key}:{nl}"));
            for (name, child) in entries {
                yaml_entry(out, name, child, indent + 2, nl)?;
            }
        }
        Node::Array { items } if !items.is_empty() => {
            out.push_str(&format!("{pad}{key}:{nl}"));
            for item in items {
                out.push_str(&format!("{pad}  - {}{nl}", yaml_text(item, None)?));
            }
        }
        scalar => out.push_str(&format!("{pad}{key}: {}{nl}", yaml_text(scalar, None)?)),
    }
    Ok(())
}
/// `- item` at `indent`; a row keeps its first key on the line of the dash.
fn yaml_item(out: &mut String, value: &Node, indent: usize, nl: &str) -> Result<()> {
    let pad = " ".repeat(indent);
    match value {
        Node::Table { entries } if !entries.is_empty() => {
            let mut body = String::new();
            for (name, child) in entries {
                yaml_entry(&mut body, name, child, indent + 2, nl)?;
            }
            out.push_str(&format!("{pad}- {}", &body[indent + 2..]));
        }
        scalar => out.push_str(&format!("{pad}- {}{nl}", yaml_text(scalar, None)?)),
    }
    Ok(())
}
/// Wrap `value` in one table per remaining key, so a missing parent is created with it.
fn nested(rest: &[Seg], value: Node) -> Result<Node> {
    rest.iter().rev().try_fold(value, |inner, seg| match seg {
        Seg::Key(key) => Ok(Node::Table {
            entries: vec![(key.clone(), inner)],
        }),
        Seg::Index(_) => Err(unsupported()),
    })
}
fn column(text: &str, at: usize) -> Result<usize> {
    let start = line_start(text, at);
    if text[start..at].contains('\t') {
        return Err(unsupported());
    }
    Ok(text[start..at].chars().count())
}
/// Step back over blank lines so an appended line joins the block it belongs to.
fn before_blank_lines(text: &str, mut at: usize, floor: usize) -> usize {
    while at > floor {
        let start = line_start(text, at - 1);
        if start <= floor || !text[start..at].trim().is_empty() {
            break;
        }
        at = start;
    }
    at
}
fn append_block(out: &mut String, text: &str, last: &Range<usize>, lines: &str) {
    let at = before_blank_lines(text, last.end, last.start);
    let lead = if text[..at].ends_with('\n') {
        ""
    } else {
        newline(text)
    };
    out.insert_str(at, &format!("{lead}{lines}"));
}
/// Add one entry before the closing bracket of a flow collection.
fn append_flow(out: &mut String, text: &str, whole: &Range<usize>, entry: &str) {
    let body = text[..whole.end - 1].trim_end().len();
    let empty = body == whole.start + 1;
    out.insert_str(body, &format!("{}{entry}", if empty { "" } else { ", " }));
}
fn remove_flow(out: &mut String, text: &str, entries: &[Range<usize>], index: usize) {
    let entry = &entries[index];
    // Flow entries carry their own trailing comma; the last one leaves the previous comma behind.
    let start = if index + 1 == entries.len() {
        text[..entry.start].trim_end_matches([' ', ',']).len()
    } else {
        entry.start
    };
    out.replace_range(start..entry.end, "");
}
/// Remove one line-based entry; a collection that becomes empty is written as `{}` or `[]`.
fn remove_block(
    out: &mut String,
    text: &str,
    holder: &impl AsYaml,
    entries: &[Range<usize>],
    index: usize,
    empty: &str,
) -> Result<()> {
    let entry = &entries[index];
    if entries.len() == 1 {
        let start = value_start(holder).ok_or_else(unsupported)?;
        let end = if text[..entry.end].ends_with('\n') {
            newline(text)
        } else {
            ""
        };
        out.replace_range(start..entry.end, &format!(" {empty}{end}"));
        return Ok(());
    }
    let start = line_start(text, entry.start);
    if text[start..entry.start].trim().is_empty() {
        out.replace_range(start..entry.end, "");
    } else if let Some(next) = entries.get(index + 1) {
        // The first key shares its line with the dash, so the next key moves up to it.
        out.replace_range(entry.start..next.start, "");
    } else {
        return Err(unsupported());
    }
    Ok(())
}

fn yaml_patch(text: &str, path: &[Seg], value: Option<Node>) -> Result<(String, Option<Node>)> {
    let root = yaml_root(text)?;
    let mut chain = vec![root];
    for seg in path {
        match yaml_child(&chain[chain.len() - 1], seg) {
            Some(next) => chain.push(next),
            None => break,
        }
    }
    let found = chain.len() - 1;
    let mut out = text.to_owned();
    if path.is_empty() {
        return Err(unsupported());
    }
    let nl = newline(text);
    if found == path.len() {
        let target = &chain[found];
        let Some(value) = value else {
            let entry = entry_span(target).ok_or_else(unsupported)?;
            match &chain[found - 1] {
                YamlNode::Mapping(map) => {
                    let entries = child_spans(map, SyntaxKind::MAPPING_ENTRY);
                    let index = entries
                        .iter()
                        .position(|e| *e == entry)
                        .ok_or_else(unsupported)?;
                    if map.is_flow_style() {
                        remove_flow(&mut out, text, &entries, index);
                    } else {
                        remove_block(&mut out, text, map, &entries, index, "{}")?;
                    }
                }
                YamlNode::Sequence(seq) => {
                    let entries = child_spans(seq, SyntaxKind::SEQUENCE_ENTRY);
                    let index = entries
                        .iter()
                        .position(|e| *e == entry)
                        .ok_or_else(unsupported)?;
                    if seq.is_flow_style() {
                        remove_flow(&mut out, text, &entries, index);
                    } else {
                        remove_block(&mut out, text, seq, &entries, index, "[]")?;
                    }
                }
                _ => return Err(unsupported()),
            }
            return Ok((out, None));
        };
        let YamlNode::Scalar(scalar) = target else {
            return Err(unsupported());
        };
        let raw = scalar.value();
        if matches!(value, Node::Array { .. }) || raw.starts_with(['|', '>']) || raw.contains('\n')
        {
            return Err(unsupported());
        }
        if raw.is_empty() {
            // `key:` without a value has no text to replace; the value goes after the colon.
            let at = value_start(scalar).ok_or_else(unsupported)?;
            out.insert_str(at, &format!(" {}", yaml_text(&value, None)?));
            return Ok((out, Some(value)));
        }
        let range = node_span(scalar).ok_or_else(unsupported)?;
        out.replace_range(range, &yaml_text(&value, Some(&raw))?);
        return Ok((out, Some(value)));
    }
    let Some(value) = value else {
        return Ok((out, None));
    };
    let rest = &path[found..];
    match (&chain[found], &rest[0]) {
        (YamlNode::Mapping(map), Seg::Key(key)) => {
            let inner = nested(&rest[1..], value.clone())?;
            let entries = child_spans(map, SyntaxKind::MAPPING_ENTRY);
            let whole = node_span(map).ok_or_else(unsupported)?;
            match entries.last() {
                Some(last) if !map.is_flow_style() => {
                    let mut lines = String::new();
                    yaml_entry(&mut lines, key, &inner, column(text, entries[0].start)?, nl)?;
                    append_block(&mut out, text, last, &lines);
                }
                _ => append_flow(
                    &mut out,
                    text,
                    &whole,
                    &format!("{}: {}", yaml_key(key)?, yaml_text(&inner, None)?),
                ),
            }
        }
        (YamlNode::Sequence(seq), Seg::Index(index)) => {
            let entries = child_spans(seq, SyntaxKind::SEQUENCE_ENTRY);
            if *index != entries.len() || (rest.len() == 1 && matches!(value, Node::Array { .. })) {
                return Err(unsupported());
            }
            // With further keys the new item is a row that starts with them.
            let item = nested(&rest[1..], value.clone())?;
            let whole = node_span(seq).ok_or_else(unsupported)?;
            match entries.last() {
                Some(last) if !seq.is_flow_style() => {
                    let mut lines = String::new();
                    yaml_item(&mut lines, &item, column(text, entries[0].start)?, nl)?;
                    append_block(&mut out, text, last, &lines);
                }
                // An empty document list becomes a block list, the form these files are written in.
                None if found == 0 && column(text, whole.start)? == 0 => {
                    let mut lines = String::new();
                    yaml_item(&mut lines, &item, 0, nl)?;
                    out.replace_range(whole, lines.trim_end_matches(['\r', '\n']));
                }
                _ => append_flow(&mut out, text, &whole, &yaml_text(&item, None)?),
            }
        }
        _ => return Err(unsupported()),
    }
    Ok((out, Some(value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn path(segments: &[&str]) -> Vec<Seg> {
        segments
            .iter()
            .map(|s| match s.parse::<usize>() {
                Ok(index) => Seg::Index(index),
                Err(_) => Seg::Key((*s).into()),
            })
            .collect()
    }
    fn toml(text: &str, segments: &[&str], value: Option<serde_json::Value>) -> String {
        patch(Format::Toml, text, &path(segments), value.as_ref()).unwrap()
    }
    fn yaml(text: &str, segments: &[&str], value: Option<serde_json::Value>) -> String {
        patch(Format::Yaml, text, &path(segments), value.as_ref()).unwrap()
    }

    const TOML: &str = "# 顶部说明\nmodel = \"old\" # 行尾说明\nflag = 1\ntext_flag = '1'\nratio = 1.5\nfuture_extra = { a = 1, b = \"2\" }\nroots = [\n  \"a\", # 第一项\n  \"b\",\n]\n\n# 界面\n[tui]\nanimations = false\n\n[tui.effects]\nwelcome = true\n\n[[rows]]\nname = 'lit'\n\n[[rows]]\nname = \"two\"\nwhen = 1979-05-27T07:32:00Z\n";

    #[test]
    fn toml_scalar_edits_touch_one_value_and_keep_comments_order_and_unknown_keys() {
        let next = toml(TOML, &["model"], Some(json!("new")));
        assert_eq!(next, TOML.replace("\"old\"", "\"new\""));
        let next = toml(TOML, &["tui", "animations"], Some(json!(true)));
        assert_eq!(
            next,
            TOML.replace("animations = false", "animations = true")
        );
        let next = toml(TOML, &["rows", "1", "name"], Some(json!("three")));
        assert_eq!(next, TOML.replace("\"two\"", "\"three\""));
        let next = toml(TOML, &["future_extra", "b"], Some(json!("3")));
        assert_eq!(next, TOML.replace("b = \"2\"", "b = \"3\""));
        let next = toml(TOML, &["roots", "0"], Some(json!("c")));
        assert_eq!(next, TOML.replace("\"a\", # 第一项", "\"c\", # 第一项"));
    }

    #[test]
    fn toml_flags_keep_their_number_string_and_quote_style() {
        assert_eq!(
            toml(TOML, &["flag"], Some(json!(0))),
            TOML.replace("flag = 1", "flag = 0")
        );
        assert_eq!(
            toml(TOML, &["text_flag"], Some(json!("0"))),
            TOML.replace("text_flag = '1'", "text_flag = '0'")
        );
        // A text that cannot be a literal string falls back to a basic one.
        assert_eq!(
            toml(TOML, &["text_flag"], Some(json!("it's"))),
            TOML.replace("text_flag = '1'", "text_flag = \"it's\"")
        );
        assert_eq!(
            toml(TOML, &["ratio"], Some(json!(2))),
            TOML.replace("ratio = 1.5", "ratio = 2.0")
        );
        let Node::Table { entries } = tree(Format::Toml, TOML).unwrap() else {
            panic!("root must be a table")
        };
        let kinds: Vec<_> = entries.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(kinds[1], ("flag", &Node::Integer { value: 1 }));
        assert_eq!(kinds[2], ("text_flag", &Node::String { value: "1".into() }));
        assert_eq!(kinds[3], ("ratio", &Node::Float { value: 1.5 }));
    }

    #[test]
    fn toml_new_keys_join_their_table_and_new_tables_go_to_the_end() {
        // A root key goes after the last root value, before every table header.
        assert_eq!(
            toml(TOML, &["sandbox_mode"], Some(json!("read-only"))),
            TOML.replace("]\n\n# 界面", "]\nsandbox_mode = \"read-only\"\n\n# 界面")
        );
        assert_eq!(
            toml(TOML, &["tui", "theme"], Some(json!("dark"))),
            TOML.replace(
                "animations = false\n",
                "animations = false\ntheme = \"dark\"\n"
            )
        );
        assert_eq!(
            toml(TOML, &["history", "persistence"], Some(json!("none"))),
            format!("{TOML}\n[history]\npersistence = \"none\"\n")
        );
        assert_eq!(
            toml(TOML, &["tui", "rendering", "tables"], Some(json!(false))),
            format!("{TOML}\n[tui.rendering]\ntables = false\n")
        );
        assert_eq!(
            toml(TOML, &["future_extra", "c"], Some(json!(true))),
            TOML.replace("b = \"2\" }", "b = \"2\", c = true }")
        );
        assert_eq!(
            toml(TOML, &["rows", "0", "extra"], Some(json!(1))),
            TOML.replace("name = 'lit'\n", "name = 'lit'\nextra = 1\n")
        );
        // Without root values the key lands above the first header and its own comment.
        assert_eq!(
            toml(
                "# 文件说明\n\n# 界面\n[tui]\nx = 1\n",
                &["model"],
                Some(json!("m"))
            ),
            "# 文件说明\n\nmodel = \"m\"\n\n# 界面\n[tui]\nx = 1\n"
        );
        assert_eq!(toml("", &["model"], Some(json!("m"))), "model = \"m\"\n");
        assert_eq!(
            toml("a = 1", &["tui", "x"], Some(json!(1))),
            "a = 1\n\n[tui]\nx = 1\n"
        );
        // Dotted keys stay dotted, next to the table that owns them.
        assert_eq!(
            toml("a.b = 1\n\n[t]\nx.y = 1\n", &["a", "c"], Some(json!(2))),
            "a.b = 1\na.c = 2\n\n[t]\nx.y = 1\n"
        );
        assert_eq!(
            toml(
                "a.b = 1\n\n[t]\nx.y = 1\n",
                &["t", "x", "z"],
                Some(json!(2))
            ),
            "a.b = 1\n\n[t]\nx.y = 1\nx.z = 2\n"
        );
        assert_eq!(
            toml("[a.b]\nx = 1\n", &["a", "k"], Some(json!(1))),
            "[a.b]\nx = 1\n\n[a]\nk = 1\n"
        );
        assert_eq!(
            toml("x = 1\n", &["odd name", "a.b"], Some(json!(1))),
            "x = 1\n\n[\"odd name\"]\n\"a.b\" = 1\n"
        );
    }

    #[test]
    fn toml_deleting_removes_only_the_selected_line_or_item() {
        assert_eq!(
            toml(TOML, &["model"], None),
            TOML.replace("model = \"old\" # 行尾说明\n", "")
        );
        assert_eq!(
            toml(TOML, &["tui", "animations"], None),
            TOML.replace("animations = false\n", "")
        );
        assert_eq!(
            toml(TOML, &["future_extra", "a"], None),
            TOML.replace("{ a = 1, b = \"2\" }", "{ b = \"2\" }")
        );
        assert_eq!(
            toml(TOML, &["future_extra", "b"], None),
            TOML.replace("{ a = 1, b = \"2\" }", "{ a = 1 }")
        );
        assert_eq!(
            toml(TOML, &["roots"], None),
            TOML.replace("roots = [\n  \"a\", # 第一项\n  \"b\",\n]\n", "")
        );
        assert_eq!(toml(TOML, &["missing", "key"], None), TOML);
        assert_eq!(toml("a.b = 1\nc = 2\n", &["a", "b"], None), "c = 2\n");
    }

    #[test]
    fn toml_scalar_lists_grow_and_shrink_in_place() {
        assert_eq!(
            toml(TOML, &["roots", "2"], Some(json!("c"))),
            TOML.replace("  \"b\",\n]", "  \"b\",\n  \"c\",\n]")
        );
        assert_eq!(
            toml(TOML, &["roots", "0"], None),
            TOML.replace("  \"a\", # 第一项\n", "")
        );
        assert_eq!(
            toml(TOML, &["roots", "1"], None),
            TOML.replace("  \"b\",\n", "")
        );
        let flat = "notify = [\"a\", \"b\"]\nempty = []\nopen = [\n  1,\n  2\n]\n";
        assert_eq!(
            toml(flat, &["notify", "2"], Some(json!("c"))),
            flat.replace("[\"a\", \"b\"]", "[\"a\", \"b\", \"c\"]")
        );
        assert_eq!(
            toml(flat, &["notify", "0"], None),
            flat.replace("[\"a\", \"b\"]", "[\"b\"]")
        );
        assert_eq!(
            toml(flat, &["notify", "1"], None),
            flat.replace("[\"a\", \"b\"]", "[\"a\"]")
        );
        assert_eq!(
            toml(flat, &["empty", "0"], Some(json!(1))),
            flat.replace("empty = []", "empty = [1]")
        );
        assert_eq!(
            toml(flat, &["open", "2"], Some(json!(3))),
            flat.replace("  2\n]", "  2,\n  3\n]")
        );
        assert_eq!(
            toml(flat, &["list"], Some(json!(["x", "y"]))),
            format!("{flat}list = [\"x\", \"y\"]\n")
        );
    }

    #[test]
    fn toml_named_tables_take_new_keys_and_new_siblings() {
        let text = "model = \"m\"\n\n[model_providers.relay]\nbase_url = \"https://example.invalid/v1\"\nenv_key = \"FIXTURE\"\n\n[tui]\nx = 1\n";
        // A key of an existing named table joins its body.
        assert_eq!(
            toml(
                text,
                &["model_providers", "relay", "request_max_retries"],
                Some(json!(3))
            ),
            text.replace(
                "env_key = \"FIXTURE\"\n",
                "env_key = \"FIXTURE\"\nrequest_max_retries = 3\n"
            )
        );
        // A new name gets its own header although `model_providers` has none.
        assert_eq!(
            toml(
                text,
                &["model_providers", "relay-2", "base_url"],
                Some(json!("https://example.invalid/v2"))
            ),
            format!(
                "{text}\n[model_providers.relay-2]\nbase_url = \"https://example.invalid/v2\"\n"
            )
        );
        assert_eq!(
            toml("", &["mcp_servers", "docs", "command"], Some(json!("run"))),
            "[mcp_servers.docs]\ncommand = \"run\"\n"
        );
        // A key-value table is created, grown and shrunk one key at a time.
        let created = toml(
            text,
            &["model_providers", "relay", "query_params", "api-version"],
            Some(json!("1")),
        );
        assert_eq!(
            created,
            format!("{text}\n[model_providers.relay.query_params]\napi-version = \"1\"\n")
        );
        let grown = toml(
            &created,
            &["model_providers", "relay", "query_params", "region"],
            Some(json!("test")),
        );
        assert_eq!(grown, format!("{created}region = \"test\"\n"));
        assert_eq!(
            toml(
                &grown,
                &["model_providers", "relay", "query_params", "region"],
                None
            ),
            created
        );
        let inline = "[model_providers.relay]\nquery_params = { a = \"1\" }\n";
        assert_eq!(
            toml(
                inline,
                &["model_providers", "relay", "query_params", "b"],
                Some(json!("2"))
            ),
            inline.replace("{ a = \"1\" }", "{ a = \"1\", b = \"2\" }")
        );
    }

    #[test]
    fn toml_last_list_item_leaves_the_previous_comment() {
        // The last item shares its line with the bracket; the line above keeps comma and comment.
        assert_eq!(
            toml("a = [\n  1, # one\n  2 ]\n", &["a", "1"], None),
            "a = [\n  1, # one\n]\n"
        );
        assert_eq!(
            toml("a = [\n  1, # one\n  2, ]\n", &["a", "1"], None),
            "a = [\n  1, # one\n]\n"
        );
        assert_eq!(
            toml("a = [\n  1, # one\n  2,\n]\n", &["a", "1"], None),
            "a = [\n  1, # one\n]\n"
        );
        assert_eq!(toml("a = [1, 2,]\n", &["a", "1"], None), "a = [1,]\n");
        assert_eq!(
            toml("a = [\n  1 # one\n  , 2 ]\n", &["a", "1"], None),
            "a = [\n  1 # one\n]\n"
        );
    }

    #[test]
    fn toml_keeps_crlf_line_endings() {
        let text = "model = \"a\"\r\n\r\n[tui]\r\nanimations = false\r\n";
        assert_eq!(
            toml(text, &["tui", "theme"], Some(json!("dark"))),
            "model = \"a\"\r\n\r\n[tui]\r\nanimations = false\r\ntheme = \"dark\"\r\n"
        );
        assert_eq!(
            toml(text, &["approval_policy"], Some(json!("never"))),
            "model = \"a\"\r\napproval_policy = \"never\"\r\n\r\n[tui]\r\nanimations = false\r\n"
        );
        assert_eq!(
            toml(text, &["history", "max_bytes"], Some(json!(10))),
            format!("{text}\r\n[history]\r\nmax_bytes = 10\r\n")
        );
        assert_eq!(
            toml(text, &["tui", "animations"], None),
            "model = \"a\"\r\n\r\n[tui]\r\n"
        );
    }

    #[test]
    fn toml_rejects_invalid_text_and_structural_edits_without_a_result() {
        let broken = "model = \n[tui";
        assert_eq!(tree(Format::Toml, broken).unwrap_err().code, code::TOML);
        assert_eq!(
            patch(Format::Toml, broken, &path(&["model"]), Some(&json!("x")))
                .unwrap_err()
                .code,
            code::TOML
        );
        for (segments, value) in [
            (vec!["tui"], None),
            (vec!["rows", "0"], None),
            (vec!["tui"], Some(json!("x"))),
            (vec!["model", "inner"], Some(json!("x"))),
            (vec!["rows", "2", "name"], Some(json!("x"))),
            (vec!["roots", "5"], Some(json!("x"))),
            (vec!["model"], Some(json!({"a": 1}))),
            (vec!["model"], Some(json!("two\nlines"))),
            (vec![], Some(json!("x"))),
        ] {
            let error = patch(Format::Toml, TOML, &path(&segments), value.as_ref()).unwrap_err();
            assert_eq!(error.code, code::CONFIG, "{segments:?}");
        }
    }

    #[test]
    fn toml_tree_reports_types_and_leaves_dates_and_long_text_read_only() {
        let text = "when = 1979-05-27T07:32:00Z\nnote = \"\"\"a\nb\"\"\"\nbig = 9007199254740993\n[t]\non = true\n[[rows]]\nn = 1\n";
        let tree = serde_json::to_value(tree(Format::Toml, text).unwrap()).unwrap();
        assert_eq!(
            tree,
            json!({"kind": "table", "entries": [
                ["when", {"kind": "other", "text": "1979-05-27T07:32:00Z"}],
                ["note", {"kind": "other", "text": "a\nb"}],
                ["big", {"kind": "other", "text": "9007199254740993"}],
                ["t", {"kind": "table", "entries": [["on", {"kind": "boolean", "value": true}]]}],
                ["rows", {"kind": "array", "items": [
                    {"kind": "table", "entries": [["n", {"kind": "integer", "value": 1}]]}
                ]}],
            ]})
        );
    }

    const YAML: &str = "# 顶部说明\n- id: agent-default-model # 行尾说明\n  config:\n    provider: vela\n    model: \"m-1\"\n\n# 工具\n- id: tool-web\n  disabled: false\n  config:\n    fetch: true\n    level: 1\n    text_flag: '1'\n    tags:\n      - a\n      - b\n    flow: [1, \"2\"]\n    route: {api: openai-responses, apiKeyEnv: VELA_GATEWAY_KEY}\n# 结尾\n";

    #[test]
    fn yaml_scalar_edits_touch_one_value_and_keep_comments_quotes_and_order() {
        assert_eq!(
            yaml(YAML, &["1", "config", "fetch"], Some(json!(false))),
            YAML.replace("fetch: true", "fetch: false")
        );
        assert_eq!(
            yaml(YAML, &["0", "config", "model"], Some(json!("m-2"))),
            YAML.replace("\"m-1\"", "\"m-2\"")
        );
        assert_eq!(
            yaml(YAML, &["0", "config", "provider"], Some(json!("other"))),
            YAML.replace("provider: vela", "provider: other")
        );
        // Text that YAML would read as another type is quoted.
        assert_eq!(
            yaml(YAML, &["0", "config", "provider"], Some(json!("true"))),
            YAML.replace("provider: vela", "provider: \"true\"")
        );
        assert_eq!(
            yaml(YAML, &["0", "config", "provider"], Some(json!("a: b #c"))),
            YAML.replace("provider: vela", "provider: \"a: b #c\"")
        );
        assert_eq!(
            yaml(
                YAML,
                &["1", "config", "route", "api"],
                Some(json!("anthropic-messages"))
            ),
            YAML.replace("api: openai-responses", "api: anthropic-messages")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "flow", "1"], Some(json!("3"))),
            YAML.replace("[1, \"2\"]", "[1, \"3\"]")
        );
    }

    #[test]
    fn yaml_flags_keep_their_number_string_and_quote_style() {
        assert_eq!(
            yaml(YAML, &["1", "config", "level"], Some(json!(0))),
            YAML.replace("level: 1", "level: 0")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "text_flag"], Some(json!("0"))),
            YAML.replace("text_flag: '1'", "text_flag: '0'")
        );
        assert_eq!(
            yaml(YAML, &["1", "disabled"], Some(json!(true))),
            YAML.replace("disabled: false", "disabled: true")
        );
        let tree = serde_json::to_value(tree(Format::Yaml, YAML).unwrap()).unwrap();
        let config = &tree["items"][1]["entries"][2][1]["entries"];
        assert_eq!(
            config[0],
            json!(["fetch", {"kind": "boolean", "value": true}])
        );
        assert_eq!(config[1], json!(["level", {"kind": "integer", "value": 1}]));
        assert_eq!(
            config[2],
            json!(["text_flag", {"kind": "string", "value": "1"}])
        );
        assert_eq!(
            config[4],
            json!(["flow", {"kind": "array", "items": [
                {"kind": "integer", "value": 1}, {"kind": "string", "value": "2"}
            ]}])
        );
    }

    #[test]
    fn yaml_new_keys_join_their_mapping_with_its_indentation() {
        assert_eq!(
            yaml(YAML, &["1", "config", "searchTimeoutMs"], Some(json!(3000))),
            YAML.replace(
                "VELA_GATEWAY_KEY}\n",
                "VELA_GATEWAY_KEY}\n    searchTimeoutMs: 3000\n"
            )
        );
        // The new row key stays inside its row, above the blank line and the next comment.
        assert_eq!(
            yaml(YAML, &["0", "disabled"], Some(json!(true))),
            YAML.replace("\"m-1\"\n", "\"m-1\"\n  disabled: true\n")
        );
        assert_eq!(
            yaml(
                YAML,
                &["0", "config", "reasoningEffort"],
                Some(json!("high"))
            ),
            YAML.replace("\"m-1\"\n", "\"m-1\"\n    reasoningEffort: high\n")
        );
        assert_eq!(
            yaml(
                YAML,
                &["1", "config", "route", "displayName"],
                Some(json!("Vela"))
            ),
            YAML.replace("VELA_GATEWAY_KEY}", "VELA_GATEWAY_KEY, displayName: Vela}")
        );
        let bare = "- id: tool-fs\n- id: other\n  config: {}\n";
        assert_eq!(
            yaml(bare, &["0", "config", "applyPatch"], Some(json!(false))),
            "- id: tool-fs\n  config:\n    applyPatch: false\n- id: other\n  config: {}\n"
        );
        assert_eq!(
            yaml(bare, &["1", "config", "enabled"], Some(json!(true))),
            "- id: tool-fs\n- id: other\n  config: {enabled: true}\n"
        );
        let settings = "tool-web:\n  fetch: true\nempty:\n";
        assert_eq!(
            yaml(settings, &["tool-web", "limits", "max"], Some(json!(2))),
            "tool-web:\n  fetch: true\n  limits:\n    max: 2\nempty:\n"
        );
        assert_eq!(
            yaml(settings, &["empty"], Some(json!("x"))),
            "tool-web:\n  fetch: true\nempty: x\n"
        );
        assert_eq!(yaml("a: 1", &["b"], Some(json!(2))), "a: 1\nb: 2\n");
    }

    #[test]
    fn yaml_deleting_removes_only_the_selected_entry() {
        assert_eq!(
            yaml(YAML, &["1", "disabled"], None),
            YAML.replace("  disabled: false\n", "")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "route", "api"], None),
            YAML.replace("{api: openai-responses, apiKeyEnv", "{apiKeyEnv")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "route", "apiKeyEnv"], None),
            YAML.replace(", apiKeyEnv: VELA_GATEWAY_KEY", "")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "tags", "0"], None),
            YAML.replace("      - a\n", "")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "flow", "0"], None),
            YAML.replace("[1, \"2\"]", "[\"2\"]")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "flow", "1"], None),
            YAML.replace("[1, \"2\"]", "[1]")
        );
        // A multi-line flow list keeps the comment of the line above the removed last item.
        assert_eq!(
            yaml("a: [\n  1, # one\n  2 ]\n", &["a", "1"], None),
            "a: [\n  1, # one\n]\n"
        );
        assert_eq!(
            yaml("a: [\n  1, # one\n  2, ]\n", &["a", "1"], None),
            "a: [\n  1, # one\n]\n"
        );
        // The last key of a mapping leaves an explicit empty mapping, not a null.
        let single = "- id: ui\n  config:\n    enabled: true\n  name: x\n";
        assert_eq!(
            yaml(single, &["0", "config", "enabled"], None),
            "- id: ui\n  config: {}\n  name: x\n"
        );
        let list = "tags:\n  - only\nnext: 1\n";
        assert_eq!(yaml(list, &["tags", "0"], None), "tags: []\nnext: 1\n");
        assert_eq!(yaml(YAML, &["1", "config", "missing"], None), YAML);
    }

    #[test]
    fn yaml_scalar_lists_grow_in_place() {
        assert_eq!(
            yaml(YAML, &["1", "config", "tags", "2"], Some(json!("c"))),
            YAML.replace("      - b\n", "      - b\n      - c\n")
        );
        assert_eq!(
            yaml(YAML, &["1", "config", "flow", "2"], Some(json!(3))),
            YAML.replace("[1, \"2\"]", "[1, \"2\", 3]")
        );
        assert_eq!(
            yaml("list: []\n", &["list", "0"], Some(json!("a"))),
            "list: [a]\n"
        );
        assert_eq!(
            yaml("a: 1\n", &["list"], Some(json!(["x", "true"]))),
            "a: 1\nlist:\n  - x\n  - \"true\"\n"
        );
        assert_eq!(
            yaml("- a\n- b\n# 结尾\n", &["2"], Some(json!("c"))),
            "- a\n- b\n- c\n# 结尾\n"
        );
    }

    #[test]
    fn yaml_rows_are_added_by_their_first_key() {
        // An empty patch file turns into the block list these files are written in.
        assert_eq!(
            yaml("# 说明\n[]\n", &["0", "id"], Some(json!("tool-web"))),
            "# 说明\n- id: tool-web\n"
        );
        assert_eq!(
            yaml("[]\r\n", &["0", "id"], Some(json!("tool-web"))),
            "- id: tool-web\r\n"
        );
        let rows = "- id: a\n  config:\n    on: true\n\n# 结尾\n";
        let added = yaml(rows, &["1", "id"], Some(json!("tool-web")));
        assert_eq!(
            added,
            "- id: a\n  config:\n    on: true\n- id: tool-web\n\n# 结尾\n"
        );
        assert_eq!(
            yaml(&added, &["1", "disabled"], Some(json!(true))),
            "- id: a\n  config:\n    on: true\n- id: tool-web\n  disabled: true\n\n# 结尾\n"
        );
        assert_eq!(
            yaml("- id: a\n", &["1", "config", "fetch"], Some(json!(false))),
            "- id: a\n- config:\n    fetch: false\n"
        );
        assert_eq!(
            yaml("rows: []\n", &["rows", "0", "id"], Some(json!("x"))),
            "rows: [{id: x}]\n"
        );
        let beyond = patch(
            Format::Yaml,
            "- id: a\n",
            &path(&["2", "id"]),
            Some(&json!("x")),
        );
        assert_eq!(beyond.unwrap_err().code, code::CONFIG);
    }

    #[test]
    fn yaml_keeps_crlf_line_endings() {
        let text = "- id: a\r\n  config:\r\n    on: true\r\n- id: b\r\n";
        assert_eq!(
            yaml(text, &["0", "config", "on"], Some(json!(false))),
            "- id: a\r\n  config:\r\n    on: false\r\n- id: b\r\n"
        );
        assert_eq!(
            yaml(text, &["0", "config", "level"], Some(json!(2))),
            "- id: a\r\n  config:\r\n    on: true\r\n    level: 2\r\n- id: b\r\n"
        );
        assert_eq!(
            yaml(text, &["1", "disabled"], Some(json!(true))),
            "- id: a\r\n  config:\r\n    on: true\r\n- id: b\r\n  disabled: true\r\n"
        );
        assert_eq!(
            yaml(text, &["0", "config", "on"], None),
            "- id: a\r\n  config: {}\r\n- id: b\r\n"
        );
    }

    #[test]
    fn yaml_rejects_invalid_text_and_structural_edits_without_a_result() {
        assert_eq!(
            tree(Format::Yaml, "a: 1\n---\nb: 2\n").unwrap_err().code,
            code::CONFIG
        );
        assert_eq!(
            tree(Format::Yaml, "just text\n").unwrap_err().code,
            code::CONFIG
        );
        let special = "note: |\n  line1\n  line2\nref: &anchor 1\nalias: *anchor\ntagged: !tag v\nhex: 0x1F\nnone: ~\n";
        let shown = serde_json::to_value(tree(Format::Yaml, special).unwrap()).unwrap();
        for index in [0, 2, 3, 4, 5] {
            assert_eq!(shown["entries"][index][1]["kind"], "other", "{index}");
        }
        for (segments, value) in [
            (vec!["note"], Some(json!("x"))),
            (vec!["alias"], Some(json!("x"))),
            (vec!["tagged"], Some(json!("x"))),
            (vec!["ref", "inner"], Some(json!("x"))),
            (vec!["note"], Some(json!({"a": 1}))),
            (vec![], Some(json!("x"))),
        ] {
            let error = patch(Format::Yaml, special, &path(&segments), value.as_ref()).unwrap_err();
            assert_eq!(error.code, code::CONFIG, "{segments:?}");
        }
        // The only row of the document cannot be removed here.
        assert!(patch(Format::Yaml, "- id: a\n", &path(&["0"]), None).is_err());
        assert_eq!(
            yaml("none: ~\nnext: 1\n", &["none"], Some(json!(true))),
            "none: true\nnext: 1\n"
        );
    }

    #[test]
    fn credentials_are_hidden_in_the_tree_and_cannot_be_patched() {
        for key in [
            "api_key",
            "apiKey",
            "APIKEY",
            "OPENAI_API_KEY",
            "experimental_bearer_token",
            "ANTHROPIC_AUTH_TOKEN",
            "accessToken",
            "client_secret",
            "password",
            "Authorization",
            "http_headers",
            "aws_credentials",
        ] {
            assert!(sensitive(key), "{key}");
        }
        for key in [
            "keys",
            "key",
            "kitty_keyboard",
            "voice_keybind_enabled",
            "tool_output_token_limit",
            "turn_token_count",
            "maxInlineTokens",
            "cli_auth_credentials_store",
            "apiKeyEnv",
            "requires_openai_auth",
            "model",
        ] {
            assert!(!sensitive(key), "{key}");
        }

        let secret = "fixture-secret-token-value";
        let toml_text = format!(
            "model = \"m\"\ntool_output_token_limit = 9\n\n[model_providers.custom]\nname = \"x\"\nexperimental_bearer_token = \"{secret}\"\nhttp_headers = {{ \"X-Trace\" = \"{secret}\", on = true }}\n\n[mcp_servers.a.env]\nAPI_KEY = \"\"\"{secret}\n{secret}\"\"\"\nFLAG = \"1\"\n"
        );
        let shown = read_config_tree(Format::Toml, toml_text.clone()).unwrap();
        let wire = serde_json::to_string(&shown).unwrap();
        assert!(!wire.contains(secret));
        assert_eq!(wire.matches("{\"kind\":\"secret\"}").count(), 4);
        assert!(wire.contains("[\"tool_output_token_limit\",{\"kind\":\"integer\",\"value\":9}]"));
        assert!(wire.contains("[\"FLAG\",{\"kind\":\"string\",\"value\":\"1\"}]"));

        let yaml_text = format!(
            "- id: llm-pi-ai\n  config:\n    providers:\n      vela: {{api: openai-responses, apiKey: {secret}, apiKeyEnv: VELA_GATEWAY_KEY}}\n      dup: {{a: {secret}, a: 2}}\n      tagged: !env {secret}\n"
        );
        let shown = read_config_tree(Format::Yaml, yaml_text.clone()).unwrap();
        let wire = serde_json::to_string(&shown).unwrap();
        assert!(!wire.contains(secret));
        assert!(wire.contains("[\"apiKey\",{\"kind\":\"secret\"}]"));
        assert!(
            wire.contains("[\"apiKeyEnv\",{\"kind\":\"string\",\"value\":\"VELA_GATEWAY_KEY\"}]")
        );

        for (format, text, segments) in [
            (
                Format::Toml,
                &toml_text,
                vec!["model_providers", "custom", "experimental_bearer_token"],
            ),
            (
                Format::Toml,
                &toml_text,
                vec!["model_providers", "custom", "http_headers", "on"],
            ),
            (
                Format::Yaml,
                &yaml_text,
                vec!["0", "config", "providers", "vela", "apiKey"],
            ),
        ] {
            for value in [Some(json!("x")), None] {
                let error = patch(format, text, &path(&segments), value.as_ref()).unwrap_err();
                assert_eq!(error.code, code::CONFIG, "{segments:?}");
                assert!(!error.message.contains(secret));
            }
        }
        // Their neighbours stay editable.
        assert_eq!(
            toml(
                &toml_text,
                &["model_providers", "custom", "name"],
                Some(json!("y"))
            ),
            toml_text.replace("name = \"x\"", "name = \"y\"")
        );
    }

    #[test]
    fn command_arguments_arrive_as_the_web_view_sends_them() {
        // Tauri parses the invoke payload into one JSON value and reads each argument from it.
        let payload: serde_json::Value = serde_json::from_str(
            "{\"format\":\"yaml\",\"path\":[1,\"config\",\"1\",\"level\"],\"value\":0,\"text\":\"1\",\"ratio\":1.5,\"none\":null}",
        )
        .unwrap();
        let format = Format::deserialize(&payload["format"]).unwrap();
        let segments = Vec::<Seg>::deserialize(&payload["path"]).unwrap();
        assert_eq!(format, Format::Yaml);
        assert!(matches!(
            segments.as_slice(),
            [Seg::Index(1), Seg::Key(config), Seg::Key(one), Seg::Key(level)]
                if config == "config" && one == "1" && level == "level"
        ));
        let read = |key: &str| Option::<serde_json::Value>::deserialize(&payload[key]).unwrap();
        assert_eq!(
            requested(&read("value").unwrap(), false).unwrap(),
            Node::Integer { value: 0 }
        );
        assert_eq!(
            requested(&read("text").unwrap(), false).unwrap(),
            Node::String { value: "1".into() }
        );
        assert_eq!(
            requested(&read("ratio").unwrap(), false).unwrap(),
            Node::Float { value: 1.5 }
        );
        assert!(read("none").is_none());
        assert!(read("missing").is_none());
    }
}
