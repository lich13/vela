//! Codex reads `model_catalog_json` once, when it starts. While the gateway
//! runs with the switch on, Vela keeps that file and a top-level key naming it.
//! Only a key naming Vela's own file is Vela's to change.
use crate::errors::code;
use crate::storage::{self, AppError, Result};
use std::path::{Path, PathBuf};
use toml_edit::{Document, Item};

const KEY: &str = "model_catalog_json";
const LIMIT: u64 = 16 * 1024 * 1024;

fn error(message: &str) -> AppError {
    AppError::new(code::CONFIG, message)
}
pub fn path(home: &Path) -> PathBuf {
    home.join("vela-models.json")
}
/// The catalog Vela wrote last, kept across stops as the fallback for an
/// upstream that cannot be reached.
pub fn previous(home: &Path) -> Option<Vec<u8>> {
    storage::read_user_bounded(&path(home), LIMIT)
        .ok()
        .flatten()
}
fn patch(text: &str, file: &str, apply: bool) -> Result<String> {
    let doc = Document::parse(text)
        .map_err(|_| AppError::new(code::TOML, "配置不是有效的 TOML，请先修复配置"))?;
    let profile = doc
        .get("profile")
        .and_then(Item::as_str)
        .and_then(|name| doc.get("profiles")?.get(name)?.get(KEY));
    let Some((key, item)) = doc.as_table().get_key_value(KEY) else {
        if !apply {
            return Ok(text.to_owned());
        }
        if profile.is_some() {
            return Err(error(
                "当前 profile 已设置 model_catalog_json，未接管模型列表",
            ));
        }
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let at = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let line = format!(
            "{KEY} = {}{newline}",
            toml_edit::Value::from(file.to_owned())
        );
        return Ok(format!("{}{line}{}", &text[..at], &text[at..]));
    };
    if item.as_str() != Some(file) {
        return if apply {
            Err(error(
                "config.toml 已设置其他 model_catalog_json，未接管模型列表",
            ))
        } else {
            Ok(text.to_owned())
        };
    }
    if apply {
        return if profile.is_some() {
            Err(error(
                "当前 profile 已设置 model_catalog_json，未接管模型列表",
            ))
        } else {
            Ok(text.to_owned())
        };
    }
    let (Some(key), Some(value)) = (key.span(), item.as_value().and_then(|v| v.span())) else {
        return Err(AppError::new(code::TOML, "无法定位配置字段"));
    };
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let line = text[..key.start].rfind('\n').map_or(start, |i| i + 1);
    let rest = text[value.end..].trim_start_matches([' ', '\t']);
    let after = text.len() - rest.len();
    // A trailing comment stays on its line; otherwise the whole line goes.
    let span = if let Some(rest) = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
    {
        line..text.len() - rest.len()
    } else if rest.is_empty() {
        line..after
    } else {
        key.start..after
    };
    let mut output = text.to_owned();
    output.replace_range(span, "");
    Ok(output)
}
fn config(home: &Path) -> Result<Option<(PathBuf, Vec<u8>)>> {
    let path = home.join("config.toml");
    Ok(storage::read_user_optional(&path)?.map(|raw| (path, raw)))
}
fn patched(home: &Path, raw: &[u8], apply: bool) -> Result<Option<String>> {
    let text = std::str::from_utf8(raw).map_err(|_| error("配置不是 UTF-8"))?;
    let file = path(home);
    let file = file
        .to_str()
        .ok_or_else(|| error("Codex 目录路径不是 UTF-8"))?;
    let output = patch(text, file, apply)?;
    Ok((output != text).then_some(output))
}
fn save(config: &Path, raw: &[u8], output: Option<String>) -> Result<bool> {
    let Some(output) = output else {
        return Ok(false);
    };
    storage::atomic_write_user(
        config,
        output.as_bytes(),
        Some(&storage::revision(Some(raw))),
    )?;
    Ok(true)
}
/// Writes the catalog, then names it in config.toml. Returns whether
/// anything Codex reads changed.
pub fn apply(home: &Path, catalog: &[u8]) -> Result<bool> {
    let (config, raw) = config(home)?.ok_or_else(|| error("缺少 config.toml，未接管模型列表"))?;
    // A key that belongs to the user is refused before anything is written.
    let output = patched(home, &raw, true)?;
    let changed = previous(home).as_deref() != Some(catalog);
    if changed {
        storage::atomic_write_user(&path(home), catalog, None)?;
    }
    Ok(save(&config, &raw, output)? || changed)
}
/// Withdraws the key. The file stays for the next start unless `forget`.
pub fn clear(home: &Path, forget: bool) -> Result<bool> {
    let changed = match config(home)? {
        Some((config, raw)) => {
            let output = patched(home, &raw, false)?;
            save(&config, &raw, output)?
        }
        None => false,
    };
    if forget {
        match std::fs::remove_file(path(home)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(storage::io_error(e)),
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "/fixture/codex/vela-models.json";

    #[test]
    fn key_is_added_and_removed_without_touching_other_bytes() {
        for text in [
            "# keep\nmodel = \"fixture\"\n\n[model_providers.custom]\nname = \"x\"\n",
            "model = \"fixture\"\r\n[features]\r\nfixture = true\r\n",
            "\u{feff}model = \"fixture\"\n",
            "",
        ] {
            let applied = patch(text, FILE, true).unwrap();
            let doc: toml::Value = toml::from_str(applied.trim_start_matches('\u{feff}')).unwrap();
            assert_eq!(doc[KEY].as_str(), Some(FILE), "{text:?}");
            assert_eq!(patch(&applied, FILE, true).unwrap(), applied);
            assert_eq!(patch(&applied, FILE, false).unwrap(), text, "{text:?}");
            assert_eq!(patch(text, FILE, false).unwrap(), text);
        }
        let commented = format!("{KEY} = \"{FILE}\" # note\nmodel = \"fixture\"\n");
        assert_eq!(
            patch(&commented, FILE, false).unwrap(),
            "# note\nmodel = \"fixture\"\n"
        );
        let last = format!("model = \"fixture\"\n{KEY} = \"{FILE}\"");
        assert_eq!(patch(&last, FILE, false).unwrap(), "model = \"fixture\"\n");
    }

    #[test]
    fn a_catalog_the_user_set_is_left_alone() {
        let other = "model_catalog_json = \"/fixture/own.json\"\n";
        assert!(patch(other, FILE, true).is_err());
        assert_eq!(patch(other, FILE, false).unwrap(), other);
        let profile =
            "profile = \"work\"\n[profiles.work]\nmodel_catalog_json = \"/fixture/own.json\"\n";
        assert!(patch(profile, FILE, true).is_err());
        assert!(patch("model = [", FILE, true).is_err());
    }

    #[test]
    fn files_follow_the_switch_and_keep_the_last_catalog_across_stops() {
        let home = tempfile::tempdir().unwrap();
        assert!(apply(home.path(), b"{}").is_err());
        assert!(previous(home.path()).is_none());
        let config = home.path().join("config.toml");
        std::fs::write(&config, "model = \"fixture\"\n").unwrap();
        assert!(apply(home.path(), b"{\"models\":[]}").unwrap());
        assert!(!apply(home.path(), b"{\"models\":[]}").unwrap());
        let text = std::fs::read_to_string(&config).unwrap();
        let doc: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(doc[KEY].as_str(), path(home.path()).to_str());
        assert!(clear(home.path(), false).unwrap());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "model = \"fixture\"\n"
        );
        assert_eq!(previous(home.path()).unwrap(), b"{\"models\":[]}");
        assert!(!clear(home.path(), true).unwrap());
        assert!(previous(home.path()).is_none());
        // Another catalog is refused before Vela's file is written.
        std::fs::write(&config, "model_catalog_json = \"/fixture/own.json\"\n").unwrap();
        assert!(apply(home.path(), b"{}").is_err());
        assert!(previous(home.path()).is_none());
    }
}
