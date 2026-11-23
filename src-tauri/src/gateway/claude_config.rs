//! Locate JSON values with serde's byte offsets; never reserialize unrelated settings.
use super::takeover::Pair;
use crate::errors::code;
use crate::storage::{AppError, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::ops::Range;

const KEYS: [&str; 2] = ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN"];
struct Member {
    key: String,
    start: usize,
    span: Range<usize>,
}
fn invalid() -> AppError {
    AppError::new(
        code::CLAUDE_CONFIG,
        "Claude settings.json 无效，请检查 JSON 和 env 两字段",
    )
}
fn skip(text: &str, at: &mut usize) {
    while text
        .as_bytes()
        .get(*at)
        .is_some_and(u8::is_ascii_whitespace)
    {
        *at += 1;
    }
}
fn one<T: DeserializeOwned>(text: &str, at: &mut usize) -> Result<T> {
    let mut stream = serde_json::Deserializer::from_str(&text[*at..]).into_iter::<T>();
    let value = stream.next().ok_or_else(invalid)?.map_err(|_| invalid())?;
    *at += stream.byte_offset();
    Ok(value)
}
fn object(text: &str, start: usize) -> Result<Vec<Member>> {
    if text.as_bytes().get(start) != Some(&b'{') {
        return Err(invalid());
    }
    let mut at = start + 1;
    let mut members = Vec::new();
    loop {
        skip(text, &mut at);
        if text.as_bytes().get(at) == Some(&b'}') {
            return Ok(members);
        }
        let start = at;
        let key: String = one(text, &mut at)?;
        skip(text, &mut at);
        if text.as_bytes().get(at) != Some(&b':') {
            return Err(invalid());
        }
        at += 1;
        skip(text, &mut at);
        let begin = at;
        let _: serde::de::IgnoredAny = one(text, &mut at)?;
        members.push(Member {
            key,
            start,
            span: begin..at,
        });
        skip(text, &mut at);
        match text.as_bytes().get(at) {
            Some(b',') => at += 1,
            Some(b'}') => return Ok(members),
            _ => return Err(invalid()),
        }
    }
}
fn unique<'a>(members: &'a [Member], key: &str) -> Result<Option<&'a Member>> {
    let mut found = members.iter().filter(|m| m.key == key);
    let value = found.next();
    if found.next().is_some() {
        return Err(AppError::new(
            code::CLAUDE_CONFIG,
            "Claude 配置存在重复的受管字段",
        ));
    }
    Ok(value)
}
type ObjectLocations = (Vec<Member>, Option<Range<usize>>, Vec<Member>);
fn locations(text: &str) -> Result<ObjectLocations> {
    let _: serde::de::IgnoredAny = serde_json::from_str(text).map_err(|_| invalid())?;
    let start = text.len() - text.trim_start().len();
    let root = object(text, start)?;
    let env = unique(&root, "env")?.map(|m| m.span.clone());
    let fields = env
        .as_ref()
        .map(|r| object(text, r.start))
        .transpose()?
        .unwrap_or_default();
    for key in KEYS {
        if let Some(m) = unique(&fields, key)? {
            let _: String = serde_json::from_str(&text[m.span.clone()]).map_err(|_| invalid())?;
        }
    }
    Ok((root, env, fields))
}
pub fn pair(text: &str) -> Result<Pair> {
    let (_, _, fields) = locations(text)?;
    let value = |key| {
        unique(&fields, key)?
            .map(|m| serde_json::from_str(&text[m.span.clone()]).map_err(|_| invalid()))
            .transpose()
    };
    Ok(Pair {
        base_url: value(KEYS[0])?,
        token: value(KEYS[1])?,
    })
}
fn insertion(
    text: &str,
    start: usize,
    fields: &[Member],
    values: Vec<(String, String)>,
) -> (Range<usize>, String) {
    let at = fields.last().map(|m| m.span.end).unwrap_or(start + 1);
    let multiline = text.contains('\n');
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let indent = if let Some(first) = fields.first() {
        let line = text[..first.span.start].rsplit('\n').next().unwrap_or("");
        line.chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .collect::<String>()
    } else {
        let line = text[..start].rsplit('\n').next().unwrap_or("");
        format!(
            "{}  ",
            line.chars()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .collect::<String>()
        )
    };
    let separator = if multiline {
        format!("{newline}{indent}")
    } else {
        " ".into()
    };
    let mut out = if fields.is_empty() {
        String::new()
    } else {
        ",".into()
    };
    out.push_str(&separator);
    out.push_str(
        &values
            .into_iter()
            .map(|(k, v)| format!("{}: {v}", serde_json::to_string(&k).unwrap()))
            .collect::<Vec<_>>()
            .join(&format!(",{separator}")),
    );
    (at..at, out)
}
/// Leaves the two fields as `target` has them; one it lacks is taken out.
pub fn patch(text: &str, target: &Pair) -> Result<String> {
    let (root, env, fields) = locations(text)?;
    let mut edits = Vec::new();
    let mut missing = Vec::new();
    let mut absent = Vec::new();
    for (key, value) in KEYS
        .into_iter()
        .zip([target.base_url.as_ref(), target.token.as_ref()])
    {
        let Some(value) = value else {
            absent.push(key);
            continue;
        };
        let encoded = serde_json::to_string(value).unwrap();
        if let Some(m) = unique(&fields, key)? {
            edits.push((m.span.clone(), encoded));
        } else {
            missing.push((key.to_owned(), encoded));
        }
    }
    if !missing.is_empty() {
        if let Some(env) = env {
            edits.push(insertion(text, env.start, &fields, missing));
        } else {
            let value = format!(
                "{{{}}}",
                missing
                    .into_iter()
                    .map(|(k, v)| format!("{}: {v}", serde_json::to_string(&k).unwrap()))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            edits.push(insertion(
                text,
                text.len() - text.trim_start().len(),
                &root,
                vec![("env".into(), value)],
            ));
        }
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_owned();
    for (range, value) in edits {
        out.replace_range(range, &value);
    }
    for key in absent {
        out = patch_env_value(&out, key, None)?;
    }
    Ok(out)
}

pub const HINT_HEADERS: &str = "CLAUDE_CODE_GATEWAY_HINT_HEADERS";
pub const STREAM_WATCHDOG: &str = "CLAUDE_ENABLE_STREAM_WATCHDOG";

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct HintSnapshot {
    before: Value,
    existed: bool,
    #[serde(default)]
    watchdog_before: Value,
    #[serde(default)]
    watchdog_existed: bool,
    #[serde(default)]
    watchdog_managed: bool,
}
fn env_value(text: &str, key: &str) -> Result<Option<Value>> {
    let (_, _, fields) = locations(text)?;
    unique(&fields, key)?
        .map(|m| serde_json::from_str(&text[m.span.clone()]).map_err(|_| invalid()))
        .transpose()
}
fn patch_env_value(text: &str, key: &str, target: Option<&Value>) -> Result<String> {
    let current = env_value(text, key)?;
    if current.as_ref() == target {
        return Ok(text.to_owned());
    }
    let (_, env, fields) = locations(text)?;
    let mut out = text.to_owned();
    if let Some(member) = unique(&fields, key)? {
        if let Some(value) = target {
            out.replace_range(member.span.clone(), &value.to_string());
        } else {
            let index = fields.iter().position(|m| m.start == member.start).unwrap();
            let span = if let Some(next) = fields.get(index + 1) {
                member.start..next.start
            } else if index > 0 {
                fields[index - 1].span.end..member.span.end
            } else {
                member.start..member.span.end
            };
            out.replace_range(span, "");
        }
    } else if let Some(value) = target {
        // The managed address/token patch creates env before this patch.
        let (span, inserted) = insertion(
            text,
            env.ok_or_else(invalid)?.start,
            &fields,
            vec![(key.into(), value.to_string())],
        );
        out.replace_range(span, &inserted);
    }
    Ok(out)
}
impl HintSnapshot {
    pub fn new(text: &str) -> Result<Self> {
        let before = env_value(text, HINT_HEADERS)?;
        let watchdog_before = env_value(text, STREAM_WATCHDOG)?;
        Ok(Self {
            existed: before.is_some(),
            before: before.unwrap_or(Value::Null),
            watchdog_existed: watchdog_before.is_some(),
            watchdog_before: watchdog_before.unwrap_or(Value::Null),
            watchdog_managed: true,
        })
    }
    pub fn patch(&self, text: &str, apply: bool) -> Result<String> {
        let hint_managed = Value::String("1".into());
        let watchdog_managed = Value::String("0".into());
        let mut out = if apply {
            patch_env_value(text, HINT_HEADERS, Some(&hint_managed))?
        } else if env_value(text, HINT_HEADERS)?.as_ref() == Some(&hint_managed) {
            patch_env_value(text, HINT_HEADERS, self.existed.then_some(&self.before))?
        } else {
            text.to_owned()
        };
        if self.watchdog_managed {
            if apply {
                out = patch_env_value(&out, STREAM_WATCHDOG, Some(&watchdog_managed))?;
            } else if env_value(&out, STREAM_WATCHDOG)?.as_ref() == Some(&watchdog_managed) {
                out = patch_env_value(
                    &out,
                    STREAM_WATCHDOG,
                    self.watchdog_existed.then_some(&self.watchdog_before),
                )?;
            }
        }
        Ok(out)
    }
}

// Report only identifiable overrides, never their values.
pub fn warning(bytes: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut keys = Vec::new();
    for key in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        if value
            .get("env")
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty() && v != "0")
        {
            keys.push(key);
        }
    }
    if value.get("apiKeyHelper").is_some() {
        keys.push("apiKeyHelper");
    }
    for key in KEYS {
        if std::env::var(key).is_ok_and(|v| !v.is_empty()) {
            keys.push(key);
        }
    }
    (!keys.is_empty()).then(|| format!("检测到其他认证来源：{}", keys.join("、")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_two_value_replacement_and_missing_containers() {
        let old = "{\r\n  \"env\": {\"ANTHROPIC_BASE_URL\":\"old\", \"keep\": [1,{\"n\":true}], \"ANTHROPIC_AUTH_TOKEN\": \"key\"},\r\n  \"permissions\": {\"allow\": []}\r\n}\r\n";
        let target = Pair::new("https://example.invalid/site", "fixture-new");
        assert_eq!(
            patch(old, &target).unwrap(),
            old.replace("\"old\"", "\"https://example.invalid/site\"")
                .replace("\"key\"", "\"fixture-new\"")
        );
        for text in [
            "{}",
            "{\"model\":\"untouched\"}",
            "{\"env\": {}}",
            "{\"env\":{\"ANTHROPIC_BASE_URL\":\"old\"}}",
            "{\n  \"env\": {\n    \"OTHER\": \"keep\"\n  }\n}\n",
        ] {
            let out = patch(text, &target).unwrap();
            assert!(pair(&out).unwrap() == target);
        }
    }
    #[test]
    fn rejects_invalid_duplicate_or_non_string_controlled_fields() {
        for text in [
            "[]",
            "{",
            "{\"env\":null}",
            "{\"env\":{},\"env\":{}}",
            "{\"env\":{\"ANTHROPIC_AUTH_TOKEN\":0}}",
            "{\"env\":{\"ANTHROPIC_AUTH_TOKEN\":\"a\",\"ANTHROPIC_AUTH_TOKEN\":\"b\"}}",
        ] {
            assert!(patch(text, &Pair::new("a", "b")).is_err(), "{text}");
        }
    }
}
