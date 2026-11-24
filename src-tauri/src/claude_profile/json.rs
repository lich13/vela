//! Top-level field edits preserve every unrelated byte, including CRLF/indent.
use crate::errors::code;
use crate::gateway::claude_config::{object, removal, Member};
use crate::storage::{AppError, Result};
use serde::de::IgnoredAny;
fn invalid() -> AppError {
    AppError::new(code::CLAUDE_PROFILE, "Claude 配置结构无效")
}
fn members(text: &str) -> Result<Vec<Member>> {
    object(text, text.len() - text.trim_start().len(), invalid)
}
pub fn field(text: &str, key: &str) -> Result<Option<String>> {
    Ok(members(text)?
        .iter()
        .find(|m| m.key == key)
        .map(|m| text[m.span.clone()].to_owned()))
}
pub fn set(text: &str, key: &str, value: Option<&str>) -> Result<String> {
    let fields = members(text)?;
    if let Some(value) = value {
        let _: IgnoredAny = serde_json::from_str(value).map_err(|_| invalid())?;
    }
    let mut out = text.to_owned();
    if let Some(i) = fields.iter().position(|m| m.key == key) {
        if let Some(value) = value {
            out.replace_range(fields[i].span.clone(), value);
        } else {
            out.replace_range(removal(&fields, i), "");
        }
    } else if let Some(value) = value {
        let at = fields
            .last()
            .map(|m| m.span.end)
            .unwrap_or(text.len() - text.trim_start().len() + 1);
        let sep = if fields.is_empty() { "" } else { "," };
        let newline = if text.contains("\r\n") {
            "\r\n"
        } else if text.contains('\n') {
            "\n"
        } else {
            " "
        };
        let insertion = format!(
            "{sep}{newline}  {}: {value}",
            serde_json::to_string(key).map_err(|_| invalid())?
        );
        out.insert_str(at, &insertion);
    }
    Ok(out)
}
