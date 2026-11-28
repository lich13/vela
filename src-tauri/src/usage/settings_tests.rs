use super::*;
use std::fs;

fn assert_default(settings: &Settings) {
    assert!(settings.recording);
    assert!(settings.auto_sync);
    assert_eq!(settings.refresh_seconds, 30);
    assert_eq!(settings.multiplier, "1");
    assert_eq!(settings.pricing_model, "response");
}

#[test]
fn missing_settings_use_defaults_without_creating_a_backup() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.json");

    let (settings, error) = load_settings(&path);

    assert_default(&settings);
    assert!(error.is_none());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn malformed_and_invalid_settings_are_preserved_with_digest_deduplication() {
    let temp = tempfile::tempdir().unwrap();
    let fixtures: [(&str, &[u8]); 2] = [
        ("malformed", b"{ synthetic invalid json"),
        ("invalid", br#"{"refreshSeconds":17}"#),
    ];

    for (name, bytes) in fixtures {
        let directory = temp.path().join(name);
        fs::create_dir(&directory).unwrap();
        let path = directory.join("settings.json");
        let backup = directory.join(format!("settings.json.corrupt-{}", storage::digest(bytes)));
        fs::write(&path, bytes).unwrap();

        for _ in 0..2 {
            let (settings, error) = load_settings(&path);
            assert_default(&settings);
            assert!(error.is_some());
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert_eq!(fs::read(&backup).unwrap(), bytes);
        }

        assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
    }
}

#[test]
fn existing_backup_is_preserved_with_the_invalid_settings() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.json");
    let bytes = b"synthetic invalid settings";
    let backup = temp
        .path()
        .join(format!("settings.json.corrupt-{}", storage::digest(bytes)));
    fs::write(&path, bytes).unwrap();
    fs::write(&backup, b"different fixture bytes").unwrap();

    let (settings, error) = load_settings(&path);

    assert_default(&settings);
    assert!(error.is_some());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(&backup).unwrap(), b"different fixture bytes");
}

#[cfg(unix)]
#[test]
fn settings_backup_symlink_is_not_followed_and_backup_failure_is_reported() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.json");
    let bytes = b"synthetic invalid settings";
    let target = temp.path().join("symlink-target.fixture");
    let backup = temp
        .path()
        .join(format!("settings.json.corrupt-{}", storage::digest(bytes)));
    fs::write(&path, bytes).unwrap();
    fs::write(&target, b"symlink target fixture").unwrap();
    symlink(&target, &backup).unwrap();

    let (settings, error) = load_settings(&path);

    assert_default(&settings);
    assert!(error.unwrap().contains("备份失败"));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(fs::symlink_metadata(&backup)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(&target).unwrap(), b"symlink target fixture");
}

#[test]
fn successful_configure_clears_only_settings_error() {
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    *service.0.settings_error.lock() = Some("stale settings error".into());
    *service.0.error.lock() = Some("unrelated service error".into());
    let settings = Settings {
        log_columns: Default::default(),
        log_number_mode: "compact".into(),
        recording: false,
        auto_sync: false,
        refresh_seconds: 5,
        multiplier: "2.5".into(),
        pricing_model: "request".into(),
        curve_split: ["relay.example.invalid".to_owned()].into(),
    };

    let state = service.configure(settings.clone()).unwrap();

    assert!(service.0.settings_error.lock().is_none());
    assert_eq!(state.error.as_deref(), Some("unrelated service error"));
    assert!(!state.settings.recording);
    assert!(!state.settings.auto_sync);
    assert_eq!(state.settings.refresh_seconds, 5);
    assert_eq!(state.settings.multiplier, "2.5");
    assert_eq!(state.settings.pricing_model, "request");
    assert_eq!(state.settings.curve_split, settings.curve_split);
    let saved = fs::read(&service.0.path).unwrap();
    let saved: Settings = serde_json::from_slice(&saved).unwrap();
    assert!(!saved.recording);
    assert!(!saved.auto_sync);
    assert_eq!(saved.refresh_seconds, 5);
    assert_eq!(saved.multiplier, "2.5");
    assert_eq!(saved.pricing_model, "request");
    assert_eq!(saved.curve_split, settings.curve_split);
    // The next run reads them back.
    let (loaded, error) = load_settings(&service.0.path);
    assert!(error.is_none());
    assert_eq!(loaded.curve_split, settings.curve_split);
}

#[test]
fn hosts_drawn_apart_are_saved_in_order_and_are_none_where_none_were_saved() {
    use serde_json::json;
    // A file saved before there were any.
    let earlier: Settings = serde_json::from_value(json!({ "refreshSeconds": 5 })).unwrap();
    assert!(earlier.curve_split.is_empty());
    assert!(earlier.validate().is_ok());
    let none = serde_json::to_value(Settings::default()).unwrap();
    assert_eq!(none["curveSplit"], json!([]));
    // Each host once, in the same order however they came.
    let chosen = json!({ "curveSplit": [
        "relay.example.invalid",
        "eu.relay.example.invalid",
        "relay.example.invalid",
    ] });
    let chosen: Settings = serde_json::from_value(chosen).unwrap();
    assert!(chosen.validate().is_ok());
    assert_eq!(
        serde_json::to_value(&chosen).unwrap()["curveSplit"],
        json!(["eu.relay.example.invalid", "relay.example.invalid"])
    );
}

#[test]
fn hosts_drawn_apart_are_limited_in_number_and_in_length() {
    let apart = |hosts: Vec<String>| Settings {
        curve_split: hosts.into_iter().collect(),
        ..Settings::default()
    };
    let host = |n: usize| format!("relay-{n}.example.invalid");
    assert!(apart((0..64).map(host).collect()).validate().is_ok());
    assert!(apart((0..65).map(host).collect()).validate().is_err());
    assert!(apart(vec!["a".repeat(253)]).validate().is_ok());
    for refused in [
        String::new(),
        "a".repeat(254),
        "relay example.invalid".into(),
        "relay.example.invalid\n".into(),
        "\u{3000}relay.example.invalid".into(),
    ] {
        let settings = apart(vec![refused.clone()]);
        assert!(settings.validate().is_err(), "{refused:?}");
    }

    // What is refused is not saved, and what was saved stays.
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    service
        .configure(apart(vec!["relay.example.invalid".into()]))
        .unwrap();
    assert!(service.configure(apart(vec![String::new()])).is_err());
    let (kept, error) = load_settings(&service.0.path);
    assert!(error.is_none());
    assert_eq!(
        kept.curve_split.into_iter().collect::<Vec<_>>(),
        ["relay.example.invalid"]
    );
}
