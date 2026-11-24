//! Antigravity CLI settings (`antigravity-cli/settings.json`), edited like the app config
//! but never created here: the CLI writes the file on its first run.
use crate::{
    configuration,
    errors::code,
    storage::{self, AppError, Result},
    Runtime,
};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

const BOOL_SETTINGS: &[&str] = &[
    "allowNonWorkspaceAccess",
    "clearScrollbackOnResize",
    "copyOnSelect",
    "disableSlashCommands",
    "enableTerminalSandbox",
    "hideActiveItems",
    "notifications",
    "showFeedbackSurvey",
    "showTips",
    "useG1Credits",
    "useTerminalCursor",
    "vimInsertFirst",
];

const STRING_SETTINGS: &[&str] = &[
    "agentMode",
    "altScreenMode",
    "artifactReviewPolicy",
    "colorScheme",
    "editor",
    "editorMode",
    "model",
    "modelProvider",
    "pickerGrouping",
    "queuedMessages",
    "runningLightSpeed",
    "toolPermission",
    "verbosity",
];

const MISSING: &str = "尚未找到 Antigravity CLI 配置文件，首次运行 agy 后生成";

fn file(home: &Path) -> PathBuf {
    home.join("antigravity-cli/settings.json")
}

fn backup(data: &Path) -> PathBuf {
    data.join("previous-antigravity-cli-settings.json")
}

#[cfg_attr(test, derive(Debug))]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    client_id: &'static str,
    text: String,
    revision: String,
    path: String,
    guarded: bool,
    can_restore: bool,
    missing: bool,
}

fn wrong(key: &str, expected: &str) -> AppError {
    AppError::new(code::CONFIG_TYPE, &format!("{key} 必须是{expected}"))
}

pub fn validate_text(text: &str) -> Result<()> {
    let value = configuration::parse_object(text)?;
    for key in BOOL_SETTINGS {
        if value.get(*key).is_some_and(|v| !v.is_boolean()) {
            return Err(wrong(key, "布尔值"));
        }
    }
    for key in STRING_SETTINGS {
        if value.get(*key).is_some_and(|v| !v.is_string()) {
            return Err(wrong(key, "字符串"));
        }
    }
    if value
        .get("historySize")
        .is_some_and(|v| v.as_i64().is_none_or(|n| n < -1))
    {
        return Err(wrong("historySize", "不小于 -1 的整数"));
    }
    if let Some(permissions) = value.get("permissions") {
        let Some(rules) = permissions.as_object() else {
            return Err(wrong("permissions", "对象"));
        };
        for list in ["allow", "ask", "deny"] {
            let strings = |v: &Value| v.as_array().is_some_and(|a| a.iter().all(Value::is_string));
            if rules.get(list).is_some_and(|v| !strings(v)) {
                return Err(wrong(&format!("permissions.{list}"), "字符串列表"));
            }
        }
    }
    for key in ["statusLine", "title"] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_object() && !v.is_boolean())
        {
            return Err(wrong(key, "对象或布尔值"));
        }
    }
    Ok(())
}

fn read_at(home: &Path, data: &Path) -> Result<Document> {
    let path = file(home);
    let raw = storage::read_optional(&path)?;
    Ok(Document {
        client_id: "antigravity",
        revision: storage::revision(raw.as_deref()),
        path: path.to_string_lossy().into(),
        guarded: false,
        can_restore: raw.is_some() && backup(data).is_file(),
        missing: raw.is_none(),
        text: String::from_utf8(raw.unwrap_or_default())
            .map_err(|_| AppError::new(code::UTF8, "配置必须使用 UTF-8 编码"))?,
    })
}

fn save_at(home: &Path, data: &Path, text: &str, expected: &str) -> Result<Document> {
    validate_text(text)?;
    let path = file(home);
    // Vela edits an existing file only; a missing one is left for the CLI to create.
    let previous =
        storage::read_optional(&path)?.ok_or_else(|| AppError::new(code::CONFIG, MISSING))?;
    if storage::digest(&previous) != expected {
        return Err(AppError::new(
            code::CONFLICT,
            "配置已被外部修改，草稿已保留",
        ));
    }
    storage::atomic_write(&backup(data), &previous, None)?;
    // The shared atomic writer compares the bytes immediately before replacement.
    storage::atomic_write(&path, text.as_bytes(), Some(expected))?;
    Ok(Document {
        client_id: "antigravity",
        text: text.to_owned(),
        revision: storage::digest(text.as_bytes()),
        path: path.to_string_lossy().into(),
        guarded: false,
        can_restore: true,
        missing: false,
    })
}

fn previous_at(data: &Path) -> Result<String> {
    let raw = storage::read_optional(&backup(data))?
        .ok_or_else(|| AppError::new(code::RESTORE, "没有可恢复的上次配置"))?;
    String::from_utf8(raw).map_err(|_| AppError::new(code::UTF8, "备份配置不是有效 UTF-8"))
}

pub fn read_antigravity_cli_config(r: &Runtime) -> Result<Document> {
    read_at(&r.antigravity_home(), &r.data)
}

pub fn save_antigravity_cli_config(
    r: &Runtime,
    text: String,
    expected_revision: String,
) -> Result<Document> {
    save_at(&r.antigravity_home(), &r.data, &text, &expected_revision)
}

pub fn read_previous_antigravity_cli_config(r: &Runtime) -> Result<String> {
    previous_at(&r.data)
}

pub fn validate_antigravity_cli_config(text: String) -> Result<()> {
    validate_text(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(home: &Path, text: &str) {
        std::fs::create_dir_all(home.join("antigravity-cli")).unwrap();
        std::fs::write(file(home), text).unwrap();
    }

    #[test]
    fn missing_file_is_reported_and_never_created() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let first = read_at(home.path(), data.path()).unwrap();
        assert!(first.missing);
        assert!(first.text.is_empty());
        assert!(!first.can_restore);
        let error = save_at(home.path(), data.path(), "{}", &first.revision).unwrap_err();
        assert_eq!(error.code, code::CONFIG);
        assert_eq!(error.message, MISSING);
        assert!(!home.path().join("antigravity-cli").exists());
        assert!(!backup(data.path()).exists());
        assert_eq!(previous_at(data.path()).unwrap_err().code, code::RESTORE);
    }

    #[test]
    fn saves_exact_text_keeps_one_backup_and_rejects_external_changes() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let initial =
            "{\r\n  \"notifications\": false,\r\n  \"future\": {\"keep\": [1, 2]}\r\n}\r\n";
        seed(home.path(), initial);
        let first = read_at(home.path(), data.path()).unwrap();
        assert!(!first.missing);
        assert_eq!(first.text, initial);
        assert!(first.path.ends_with("settings.json"));

        let next = initial.replace("false", "true");
        let saved = save_at(home.path(), data.path(), &next, &first.revision).unwrap();
        assert_eq!(saved.text, next);
        assert!(saved.can_restore);
        assert_eq!(std::fs::read_to_string(file(home.path())).unwrap(), next);
        assert_eq!(previous_at(data.path()).unwrap(), initial);

        // A stale revision keeps the file and the backup as they are.
        let error = save_at(home.path(), data.path(), "{}", &first.revision).unwrap_err();
        assert_eq!(error.code, code::CONFLICT);
        assert_eq!(read_at(home.path(), data.path()).unwrap().text, next);
        assert_eq!(previous_at(data.path()).unwrap(), initial);

        std::fs::write(file(home.path()), "{\"showTips\":false}").unwrap();
        let error = save_at(home.path(), data.path(), "{}", &saved.revision).unwrap_err();
        assert_eq!(error.code, code::CONFLICT);
        assert_eq!(
            std::fs::read_to_string(file(home.path())).unwrap(),
            "{\"showTips\":false}"
        );
    }

    #[test]
    fn failed_backup_aborts_before_overwriting_the_settings() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        seed(home.path(), "{\"showTips\":true}");
        let first = read_at(home.path(), data.path()).unwrap();
        std::fs::create_dir_all(backup(data.path())).unwrap();
        assert!(save_at(
            home.path(),
            data.path(),
            "{\"showTips\":false}",
            &first.revision
        )
        .is_err());
        assert_eq!(
            read_at(home.path(), data.path()).unwrap().text,
            "{\"showTips\":true}"
        );
    }

    #[test]
    fn validates_known_field_types_and_never_leaks_values_in_json_errors() {
        assert!(validate_text(
            "{\"$schema\":\"https://example.invalid/settings.json\",\"agentMode\":\"\",\"notifications\":true,\"historySize\":-1,\"permissions\":{\"deny\":[\"command(rm)\"]},\"statusLine\":{\"type\":\"command\",\"command\":\"x\",\"enabled\":false},\"title\":true,\"future\":{\"any\":[1]}}"
        )
        .is_ok());
        for invalid in [
            "{\"notifications\":\"yes\"}",
            "{\"vimInsertFirst\":1}",
            "{\"agentMode\":true}",
            "{\"model\":3}",
            "{\"historySize\":-2}",
            "{\"historySize\":1.5}",
            "{\"historySize\":\"2000\"}",
            "{\"permissions\":[]}",
            "{\"permissions\":{\"allow\":\"command(ls)\"}}",
            "{\"permissions\":{\"deny\":[1]}}",
            "{\"statusLine\":\"script\"}",
            "{\"title\":0}",
        ] {
            assert_eq!(
                validate_text(invalid).unwrap_err().code,
                code::CONFIG_TYPE,
                "{invalid}"
            );
        }
        let secret = "fixture-secret-token-value";
        let error = validate_text(&format!("{{\"token\":\"{secret}\",")).unwrap_err();
        assert_eq!(error.code, code::JSON);
        assert!(!error.message.contains(secret));
        assert_eq!(
            validate_text("{\"showTips\":true,\"showTips\":false}")
                .unwrap_err()
                .code,
            code::JSON
        );
    }
}
