use super::*;
use parking_lot::Mutex;
use tempfile::TempDir;

struct MockLifecycle {
    running: Mutex<bool>,
    events: Mutex<Vec<&'static str>>,
    fail_quit: bool,
    fail_launch: bool,
}

impl MockLifecycle {
    fn new(running: bool) -> Self {
        Self {
            running: Mutex::new(running),
            events: Mutex::new(Vec::new()),
            fail_quit: false,
            fail_launch: false,
        }
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().clone()
    }
}

impl AppLifecycle for MockLifecycle {
    fn is_running(&self) -> Result<bool> {
        Ok(*self.running.lock())
    }

    fn quit(&self) -> Result<()> {
        drop(AUTH_LOCK.lock());
        self.events.lock().push("quit");
        if self.fail_quit {
            return Err(AppError::new(code::PROCESS, "模拟退出 Antigravity 失败"));
        }
        *self.running.lock() = false;
        Ok(())
    }

    fn launch(&self) -> Result<()> {
        drop(AUTH_LOCK.lock());
        self.events.lock().push("launch");
        if self.fail_launch {
            return Err(AppError::new(code::PROCESS, "模拟重启 Antigravity 失败"));
        }
        *self.running.lock() = true;
        Ok(())
    }
}

fn setup_paths() -> (TempDir, Paths) {
    let tmp = TempDir::new().expect("tempdir");
    let user_home = tmp.path().join("home");
    let vela_data = tmp.path().join("vela");
    fs::create_dir_all(user_home.join(".gemini")).unwrap();
    fs::create_dir_all(&vela_data).unwrap();
    let paths = Paths::from_roots(&user_home, &vela_data);
    (tmp, paths)
}

const FIRST_LOGIN: &str = r#"{"email":"u1@example.invalid","access_token":"tok-1"}"#;
const SECOND_LOGIN: &str = r#"{"email":"u2@example.invalid","access_token":"tok-2"}"#;

/// Signs the app in to both logins in turn, so the second one is current.
/// Returns both account ids and the state after the second login.
fn two_accounts(paths: &Paths) -> (String, String, AntigravityAuthState) {
    write_private_file(&paths.oauth_file(), FIRST_LOGIN.as_bytes()).unwrap();
    let first = state_at(paths).unwrap().accounts[0].id.clone();
    write_private_file(&paths.oauth_file(), SECOND_LOGIN.as_bytes()).unwrap();
    let state = state_at(paths).unwrap();
    let second = active_account_id(&state).unwrap();
    assert_ne!(first, second);
    (first, second, state)
}

#[test]
fn oauth_auto_recognizes_disk_login_and_deduplicates_on_token_refresh() {
    let (_tmp, paths) = setup_paths();
    let oauth_v1 = serde_json::json!({
        "auth_method": "google-oauth",
        "user": { "email": "user1@example.invalid" },
        "token": { "access_token": "ya29.v1", "refresh_token": "1//r1" }
    });
    write_private_file(
        &paths.oauth_file(),
        serde_json::to_string_pretty(&oauth_v1).unwrap().as_bytes(),
    )
    .unwrap();

    let s1 = state_at(&paths).unwrap();
    assert!(s1.oauth_added);
    assert!(!serde_json::to_value(&s1)
        .unwrap()
        .as_object()
        .unwrap()
        .contains_key("oauthAdded"));
    assert!(!state_at(&paths).unwrap().oauth_added);
    assert_eq!(s1.accounts.len(), 1);
    assert!(s1.accounts[0].current);
    assert_eq!(
        s1.accounts[0].email.as_deref(),
        Some("user1@example.invalid")
    );
    let rev1 = s1.accounts[0].credential_revision.clone();

    let oauth_v2 = serde_json::json!({
        "auth_method": "google-oauth",
        "user": { "email": "User1@Example.Invalid" },
        "token": { "access_token": "ya29.v2-refreshed", "refresh_token": "1//r1" }
    });
    write_private_file(
        &paths.oauth_file(),
        serde_json::to_string_pretty(&oauth_v2).unwrap().as_bytes(),
    )
    .unwrap();

    let s2 = state_at(&paths).unwrap();
    assert!(!s2.oauth_added);
    assert_eq!(s2.accounts.len(), 1);
    assert!(s2.accounts[0].current);
    assert_ne!(s2.accounts[0].credential_revision, rev1);

    let oauth_second = serde_json::json!({
        "auth_method": "jetski-sso",
        "email": "user2@example.invalid",
        "access_token": "ya29.user2"
    });
    write_private_file(
        &paths.oauth_file(),
        serde_json::to_string_pretty(&oauth_second)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();

    let s3 = state_at(&paths).unwrap();
    assert_eq!(s3.accounts.len(), 2);
    assert_eq!(s3.accounts.iter().filter(|a| a.current).count(), 1);
    assert!(s3
        .accounts
        .iter()
        .any(|a| a.email.as_deref() == Some("user2@example.invalid") && a.current));
}

#[test]
fn oauth_validation_requires_access_or_refresh_token_and_disk_read_tolerates_corrupt_files() {
    let (_tmp, paths) = setup_paths();
    for bad in [
        serde_json::json!({ "id_token_email": "only-email@example.invalid" }),
        serde_json::json!({ "id_token": "eyJhbGciOiJub25lIn0.eyJlbWFpbCI6InVAZXhhbXBsZS5pbnZhbGlkIn0." }),
        serde_json::json!({ "oauth_token": "legacy-only" }),
    ] {
        let err = parse_oauth_metadata(&bad.to_string())
            .expect_err("must require access_token or refresh_token");
        assert_eq!(err.code, code::AUTH);
    }

    // Half-written OAuth file marks clean = false so polling retries on next tick.
    write_private_file(&paths.oauth_file(), b"{\"access_token\":\"half").unwrap();
    let (state, clean) = state_with_oauth(&paths, read_oauth(&paths, false).unwrap()).unwrap();
    assert!(!clean);
    assert!(!state.oauth_present);
    assert!(state.accounts.is_empty());

    // Complete JSON that can never be a login (no token) marks clean = true
    // so the auth stamp advances and background polling does not spin on every tick.
    write_private_file(
        &paths.oauth_file(),
        br#"{"id_token_email":"only-email@example.invalid"}"#,
    )
    .unwrap();
    let (state2, clean2) = state_with_oauth(&paths, read_oauth(&paths, false).unwrap()).unwrap();
    assert!(clean2);
    assert!(!state2.oauth_present);
    assert!(state2.accounts.is_empty());
}

#[test]
fn no_email_oauth_refresh_updates_in_place_without_overwriting_other_no_email_accounts() {
    let (_tmp, paths) = setup_paths();
    write_private_file(
        &paths.oauth_file(),
        br#"{"access_token":"ya29.no-email-1","refresh_token":"1//rt-1"}"#,
    )
    .unwrap();
    let s1 = state_at(&paths).unwrap();
    assert_eq!(s1.accounts.len(), 1);
    let id1 = s1.accounts[0].id.clone();

    // Refresh without email (even if refresh_token is omitted on refresh write) updates in place
    write_private_file(
        &paths.oauth_file(),
        br#"{"access_token":"ya29.no-email-2","refresh_token":"1//rt-1"}"#,
    )
    .unwrap();
    let s2 = state_at(&paths).unwrap();
    assert_eq!(s2.accounts.len(), 1);
    assert_eq!(s2.accounts[0].id, id1);
    assert!(s2.accounts[0].current);

    // A second no-email OAuth login with a different refresh_token must NOT overwrite id1
    write_private_file(
        &paths.oauth_file(),
        br#"{"access_token":"ya29.no-email-b","refresh_token":"1//rt-2"}"#,
    )
    .unwrap();
    let s3 = state_at(&paths).unwrap();
    assert_eq!(s3.accounts.len(), 2);
    let id2 = s3.accounts.iter().find(|a| a.id != id1).unwrap().id.clone();

    // Refreshing the currently active second no-email account without refresh_token updates id2, not id1
    write_private_file(
        &paths.oauth_file(),
        br#"{"access_token":"ya29.no-email-b-refreshed"}"#,
    )
    .unwrap();
    let s4 = state_at(&paths).unwrap();
    assert_eq!(s4.accounts.len(), 2);
    let (vault, _) = load_vault(&paths).unwrap();
    let entry1 = vault.entries.iter().find(|e| e.id == id1).unwrap();
    let entry2 = vault.entries.iter().find(|e| e.id == id2).unwrap();
    assert!(entry1.payload.contains("ya29.no-email-2"));
    assert!(entry2.payload.contains("ya29.no-email-b-refreshed"));
    assert!(vault.observed_revisions.is_empty());

    // Cap observed_revisions at 64 when deleting entries
    let mut v = vault;
    for i in 0..70 {
        remember_observed(&mut v, &format!("rev-{i}"));
    }
    assert_eq!(v.observed_revisions.len(), 64);
    assert_eq!(v.observed_revisions[0], "rev-6");
    assert_eq!(v.observed_revisions[63], "rev-69");
}

#[test]
fn a_login_that_gains_its_address_stays_one_account() {
    let (_tmp, paths) = setup_paths();
    let login = |access: &str, email: Option<&str>| {
        let mut value = serde_json::json!({
            "auth_method": "consumer",
            "token": { "access_token": access, "refresh_token": "1//fixture-new" }
        });
        if let Some(email) = email {
            value["id_token_email"] = email.into();
        }
        write_private_file(&paths.oauth_file(), value.to_string().as_bytes()).unwrap();
    };
    write_private_file(&paths.oauth_file(), FIRST_LOGIN.as_bytes()).unwrap();
    state_at(&paths).unwrap();
    // A new sign-in is written without the address; the first refresh adds it.
    login("fixture-a", None);
    let unnamed = state_at(&paths).unwrap();
    assert_eq!(unnamed.accounts.len(), 2);
    let id = active_account_id(&unnamed).unwrap();
    assert_eq!(unnamed.accounts[1].name, "OAuth 账号");
    login("fixture-b", Some("new@example.invalid"));
    let named = state_at(&paths).unwrap();
    assert_eq!(named.accounts.len(), 2);
    assert_eq!(active_account_id(&named).as_deref(), Some(id.as_str()));
    assert_eq!(named.accounts[1].name, "new@example.invalid");
    assert_eq!(
        named.accounts[1].email.as_deref(),
        Some("new@example.invalid")
    );
    // A name the user gave is kept, and a write without the address keeps both.
    let renamed = rename_account_at(&paths, &id, "Fixture", &named.revision).unwrap();
    assert_eq!(renamed.accounts[1].name, "Fixture");
    login("fixture-c", None);
    let state = state_at(&paths).unwrap();
    assert_eq!(state.accounts.len(), 2);
    assert_eq!(state.accounts[1].name, "Fixture");
    assert_eq!(
        state.accounts[1].email.as_deref(),
        Some("new@example.invalid")
    );
    assert!(state.accounts[1].current);
}

#[test]
fn signing_in_again_to_a_known_account_leaves_no_placeholder() {
    let (_tmp, paths) = setup_paths();
    let login = |refresh: &str, email: Option<&str>| {
        let mut value = serde_json::json!({
            "auth_method": "consumer",
            "token": { "access_token": "fixture-a", "refresh_token": refresh }
        });
        if let Some(email) = email {
            value["id_token_email"] = email.into();
        }
        write_private_file(&paths.oauth_file(), value.to_string().as_bytes()).unwrap();
    };
    login("1//fixture-old", Some("known@example.invalid"));
    let id = active_account_id(&state_at(&paths).unwrap()).unwrap();
    login("1//fixture-again", None);
    assert_eq!(state_at(&paths).unwrap().accounts.len(), 2);
    login("1//fixture-again", Some("known@example.invalid"));
    let state = state_at(&paths).unwrap();
    assert_eq!(state.accounts.len(), 1);
    assert_eq!(active_account_id(&state), Some(id));
}

#[test]
fn a_placeholder_left_by_an_earlier_version_joins_its_account() {
    let (_tmp, paths) = setup_paths();
    let entry = |id: &str, name: &str, email: Option<&str>, access: &str, refresh: &str| {
        serde_json::json!({
            "id": id, "name": name, "kind": "oauth", "sourceType": "consumer",
            "email": email, "updatedAt": 1,
            "payload": serde_json::json!({
                "auth_method": "consumer",
                "id_token_email": email,
                "token": { "access_token": access, "refresh_token": refresh }
            }).to_string(),
        })
    };
    let vault = serde_json::json!({
        "schema": 1,
        "entries": [
            entry("kept", "old@example.invalid", Some("old@example.invalid"), "fixture-a", "1//fixture-old"),
            entry("placeholder", "OAuth 账号", None, "fixture-b", "1//fixture-new"),
            entry("named", "new@example.invalid", Some("new@example.invalid"), "fixture-c", "1//fixture-new"),
            // Unnamed and nobody else's: it stays.
            entry("alone", "OAuth 账号", None, "fixture-d", "1//fixture-alone"),
        ],
        "activeOauthId": "placeholder",
        "activations": [
            { "accountId": "kept", "sinceMs": 1 },
            { "accountId": "placeholder", "sinceMs": 2 },
            { "accountId": "kept", "sinceMs": 3 },
        ],
    });
    write_private_file(&paths.vault_file(), vault.to_string().as_bytes()).unwrap();
    let state = state_at(&paths).unwrap();
    assert_eq!(
        state
            .accounts
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["kept", "named", "alone"]
    );
    let (vault, _) = load_vault(&paths).unwrap();
    assert_eq!(vault.active_oauth_id.as_deref(), Some("named"));
    assert_eq!(
        vault
            .activations
            .iter()
            .map(|s| s.account_id.as_str())
            .collect::<Vec<_>>(),
        ["kept", "named", "kept"]
    );
    assert_eq!(state.cli_sessions, 0);
    assert_eq!(
        count_running_agy_ps("/Users/fixture/.local/bin/agy\nagy\n/usr/bin/agy-helper\n/Applications/Antigravity.app/Contents/MacOS/Antigravity\n"),
        2
    );
}

#[test]
fn auth_state_serializes_exactly_the_frontend_fields() {
    let (_tmp, paths) = setup_paths();
    write_private_file(&paths.oauth_file(), FIRST_LOGIN.as_bytes()).unwrap();
    let state = serde_json::to_value(state_at(&paths).unwrap()).unwrap();
    let keys =
        |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(&state),
        [
            "accounts",
            "appRestarted",
            "cliSessions",
            "oauthAuthMethod",
            "oauthEmail",
            "oauthPresent",
            "relaunchFailed",
            "revision"
        ]
    );
    assert_eq!(
        keys(&state["accounts"][0]),
        [
            "credentialRevision",
            "current",
            "email",
            "id",
            "name",
            "sourceType",
            "updatedAt"
        ]
    );
}

#[test]
fn switch_account_writes_the_saved_login_and_restarts_a_running_app() {
    let (_tmp, paths) = setup_paths();
    let (first, second, before) = two_accounts(&paths);

    let lifecycle = MockLifecycle::new(true);
    let switched = switch_account_at(&paths, &lifecycle, &first, &before.revision).unwrap();
    assert!(switched.app_restarted && !switched.relaunch_failed);
    assert_eq!(lifecycle.events(), vec!["quit", "launch"]);
    assert_eq!(fs::read_to_string(paths.oauth_file()).unwrap(), FIRST_LOGIN);
    assert_eq!(switched.oauth_email.as_deref(), Some("u1@example.invalid"));
    assert_eq!(active_account_id(&switched), Some(first.clone()));
    assert_eq!(switched.accounts.iter().filter(|a| a.current).count(), 1);
    let (vault, _) = load_vault(&paths).unwrap();
    assert_eq!(vault.entries.len(), 2);
    assert_eq!(vault.active_oauth_id.as_deref(), Some(first.as_str()));
    assert_eq!(vault.activations.last().unwrap().account_id, first);

    // An app that is not running is left closed.
    let closed = MockLifecycle::new(false);
    let back = switch_account_at(&paths, &closed, &second, &switched.revision).unwrap();
    assert!(!back.app_restarted && !back.relaunch_failed);
    assert!(closed.events().is_empty());
    assert_eq!(
        fs::read_to_string(paths.oauth_file()).unwrap(),
        SECOND_LOGIN
    );
    assert_eq!(active_account_id(&back), Some(second));

    // The switch stays in place when the app cannot be reopened.
    let mut stuck = MockLifecycle::new(true);
    stuck.fail_launch = true;
    let kept = switch_account_at(&paths, &stuck, &first, &back.revision).unwrap();
    assert!(kept.relaunch_failed && !kept.app_restarted);
    assert_eq!(stuck.events(), vec!["quit", "launch"]);
    assert_eq!(active_account_id(&kept), Some(first));

    let unknown = switch_account_at(&paths, &closed, "missing", &kept.revision).unwrap_err();
    assert_eq!(unknown.code, code::ACCOUNT);
    assert!(closed.events().is_empty());
}

#[test]
fn a_failed_switch_restores_the_login_and_the_vault() {
    let (_tmp, paths) = setup_paths();
    let (_, _, before) = two_accounts(&paths);
    let vault = fs::read(paths.vault_file()).unwrap();

    let lifecycle = MockLifecycle::new(true);
    let failed = with_lifecycle(&paths, &before.revision, &lifecycle, || {
        write_oauth(&paths, FIRST_LOGIN)?;
        // Observing the new login records it in the vault.
        state_at(&paths)?;
        assert_ne!(fs::read(paths.vault_file()).unwrap(), vault);
        Err(err(code::PROCESS, "fixture failure"))
    });
    assert_eq!(failed.unwrap_err().message, "fixture failure");
    assert_eq!(
        fs::read_to_string(paths.oauth_file()).unwrap(),
        SECOND_LOGIN
    );
    assert_eq!(fs::read(paths.vault_file()).unwrap(), vault);
    assert_eq!(state_at(&paths).unwrap().revision, before.revision);
    // The app that was closed for the switch is opened again.
    assert_eq!(lifecycle.events(), vec!["quit", "launch"]);
}

#[test]
fn rename_and_delete_account_prevent_active_deletion_and_suppress_reimport() {
    let (_tmp, paths) = setup_paths();
    let (first, second, s1) = two_accounts(&paths);

    let err = delete_account_at(&paths, &second, &s1.revision).expect_err("cannot delete current");
    assert_eq!(err.code, code::CONFLICT);
    assert_eq!(state_at(&paths).unwrap().accounts.len(), 2);

    let s2 = rename_account_at(&paths, &first, " 新名称 ", &s1.revision).unwrap();
    assert!(s2
        .accounts
        .iter()
        .any(|a| a.id == first && a.name == "新名称"));
    for (id, name, expected) in [
        (first.as_str(), " ", code::NAME),
        ("missing", "X", code::ACCOUNT),
    ] {
        let err = rename_account_at(&paths, id, name, &s2.revision).unwrap_err();
        assert_eq!(err.code, expected);
    }

    // Deleting an account that is not in use leaves the app's login alone.
    let s3 = delete_account_at(&paths, &first, &s2.revision).unwrap();
    assert_eq!(s3.accounts.len(), 1);
    assert_eq!(s3.accounts[0].id, second);
    assert!(s3.accounts[0].current);
    assert_eq!(
        fs::read_to_string(paths.oauth_file()).unwrap(),
        SECOND_LOGIN
    );

    // The deleted login is not saved again when it comes back unchanged.
    write_private_file(&paths.oauth_file(), FIRST_LOGIN.as_bytes()).unwrap();
    let s4 = state_at(&paths).unwrap();
    assert!(s4.oauth_present && !s4.oauth_added);
    assert_eq!(s4.accounts.len(), 1);
    assert!(!s4.accounts[0].current);

    // Once the app is signed out, the last account can go too.
    fs::remove_file(paths.oauth_file()).unwrap();
    let s5 = state_at(&paths).unwrap();
    let s6 = delete_account_at(&paths, &second, &s5.revision).unwrap();
    assert!(s6.accounts.is_empty());
    assert!(load_vault(&paths).unwrap().0.active_oauth_id.is_none());
}

#[test]
fn revision_conflict_and_quit_failure_preserve_disk_files() {
    let (_tmp, paths) = setup_paths();
    let (first, _, s1) = two_accounts(&paths);
    let vault = fs::read(paths.vault_file()).unwrap();

    let conflict =
        rename_account_at(&paths, &first, "X", "stale-rev").expect_err("stale revision must fail");
    assert_eq!(conflict.code, code::CONFLICT);
    let conflict = delete_account_at(&paths, &first, "stale-rev").unwrap_err();
    assert_eq!(conflict.code, code::CONFLICT);
    let closed = MockLifecycle::new(false);
    let conflict = switch_account_at(&paths, &closed, &first, "stale-rev").unwrap_err();
    assert_eq!(conflict.code, code::CONFLICT);

    let mut failing = MockLifecycle::new(true);
    failing.fail_quit = true;
    let res = switch_account_at(&paths, &failing, &first, &s1.revision);
    assert_eq!(res.unwrap_err().code, code::PROCESS);
    assert_eq!(failing.events(), vec!["quit"]);
    assert_eq!(
        fs::read_to_string(paths.oauth_file()).unwrap(),
        SECOND_LOGIN
    );
    assert_eq!(fs::read(paths.vault_file()).unwrap(), vault);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_process_parser_matches_custom_install_paths_and_ignores_helpers() {
    let sample = "\
  101 /sbin/launchd
 4520 /Users/demo/Applications/Antigravity.app/Contents/MacOS/Antigravity
 4521 /Users/demo/Applications/Antigravity.app/Contents/Frameworks/Antigravity Helper (Renderer).app/Contents/MacOS/Antigravity Helper (Renderer)
 8890 /Volumes/External/Apps/Antigravity.app/Contents/MacOS/Antigravity
";
    let parsed = parse_running_antigravity_ps(sample);
    assert_eq!(
        parsed,
        vec![
            (
                4520,
                PathBuf::from("/Users/demo/Applications/Antigravity.app")
            ),
            (
                8890,
                PathBuf::from("/Volumes/External/Apps/Antigravity.app")
            ),
        ]
    );
}

#[test]
fn initial_activation_uses_current_credentials_and_preserves_existing_zero_history() {
    let (_tmp, paths) = setup_paths();
    let set_mtime = |path: &Path, ms: u64| {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(ms)))
            .unwrap();
    };
    write_private_file(&paths.oauth_file(), br#"{"auth_method":"google-oauth","user":{"email":"fixture@example.invalid"},"token":{"access_token":"fixture","refresh_token":"fixture"}}"#).unwrap();
    set_mtime(&paths.oauth_file(), 2_000);
    let state = state_at(&paths).unwrap();
    let id = active_account_id(&state).unwrap();
    let (mut vault, _) = load_vault(&paths).unwrap();
    assert_eq!(
        vault.activations,
        vec![ActivationSpan {
            account_id: id.clone(),
            since_ms: 2_000
        }]
    );
    let timeline = provider_timeline(&paths.data_dir, Some(&paths.gemini_home));
    assert_eq!(timeline.provider_at(1_999), None);
    assert_eq!(timeline.provider_at(2_000), Some(id.as_str()));
    vault.activations[0].since_ms = 0;
    save_vault(&paths, &vault).unwrap();
    state_at(&paths).unwrap();
    assert_eq!(load_vault(&paths).unwrap().0.activations[0].since_ms, 0);
    assert_eq!(
        provider_timeline(&paths.data_dir, Some(&paths.gemini_home)).provider_at(0),
        Some(id.as_str())
    );

    // A saved account the vault has not yet recorded as active takes over
    // from the time its login was written.
    write_private_file(&paths.oauth_file(), SECOND_LOGIN.as_bytes()).unwrap();
    let other = active_account_id(&state_at(&paths).unwrap()).unwrap();
    let (mut vault, _) = load_vault(&paths).unwrap();
    vault.activations.truncate(1);
    save_vault(&paths, &vault).unwrap();
    set_mtime(&paths.oauth_file(), 2_500);
    let timeline = provider_timeline(&paths.data_dir, Some(&paths.gemini_home));
    assert_eq!(timeline.provider_at(2_499), Some(id.as_str()));
    assert_eq!(timeline.provider_at(2_500), Some(other.as_str()));

    vault.activations.clear();
    save_vault(&paths, &vault).unwrap();
    let observed = now_ms();
    let timeline = provider_timeline(&paths.data_dir, None);
    assert!(timeline.spans[0].since_ms >= observed);
    assert!(timeline.spans[0].since_ms <= now_ms());
}

#[test]
fn current_account_and_timeline_follow_the_app_login() {
    let (_tmp, paths) = setup_paths();
    let (first, second, _) = two_accounts(&paths);
    let current = || {
        let identity = current_oauth_identity(&paths.data_dir, &paths.gemini_home).unwrap();
        let timeline = provider_timeline(&paths.data_dir, Some(&paths.gemini_home));
        (
            identity.map(|(id, _)| id),
            timeline.provider_at(i64::MAX).map(str::to_owned),
        )
    };
    assert_eq!(current(), (Some(second.clone()), Some(second.clone())));

    // A refreshed login is matched by its email before the vault has seen it.
    write_private_file(
        &paths.oauth_file(),
        br#"{"email":"U1@Example.Invalid","access_token":"tok-1-refreshed"}"#,
    )
    .unwrap();
    assert_eq!(current(), (Some(first.clone()), Some(first)));

    // Signed out, no account is current and usage stays with the last one.
    fs::remove_file(paths.oauth_file()).unwrap();
    assert_eq!(current(), (None, Some(second)));
}

#[test]
fn keyring_values_decode_from_both_encodings_and_round_trip() {
    let raw = r#"{"auth_method":"google-oauth","id_token_email":"user@example.invalid","token":{"access_token":"fixture","refresh_token":"fixture"}}"#;
    let large = raw.replace("fixture", &"fixture".repeat(1024));
    assert_eq!(
        decode_keyring(&encode_keyring(&large)).as_deref(),
        Some(large.as_str())
    );
    let stored = encode_keyring(raw);
    assert!(stored.starts_with("go-keyring-base64:"));
    assert_eq!(decode_keyring(&format!("{stored}\n")).as_deref(), Some(raw));
    assert_eq!(
        decode_keyring("go-keyring-encoded:7b7d").as_deref(),
        Some("{}")
    );
    // A value stored without a prefix is the credential itself.
    assert_eq!(decode_keyring(" {} \n").as_deref(), Some("{}"));
    for broken in [
        "go-keyring-base64:***",
        "go-keyring-encoded:7",
        "go-keyring-encoded:zz",
        "go-keyring-base64:",
        "",
    ] {
        assert_eq!(decode_keyring(broken), None, "{broken}");
    }
    let (method, email) = parse_oauth_metadata(&decode_keyring(&stored).unwrap()).unwrap();
    assert_eq!(method, "google-oauth");
    assert_eq!(email.as_deref(), Some("user@example.invalid"));
}

#[test]
fn keychain_stdin_command_enforces_4096_limit_and_fingerprint_cache_skips_repeat_reads() {
    let cmd = build_keychain_write_command(r#"{"access_token":"fixture"}"#).unwrap();
    assert!(
        cmd.starts_with("add-generic-password -U -s gemini -a antigravity -w go-keyring-base64:")
    );
    assert!(cmd.ends_with('\n'));
    let huge = "x".repeat(KEYCHAIN_COMMAND_LIMIT);
    let err = build_keychain_write_command(&huge).unwrap_err();
    assert_eq!(err.message, "Antigravity 登录凭据过大，无法写入钥匙串");

    let cache = Mutex::new(None);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let first = cached_keychain_read(Some((1, 100, 999)), &cache, || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some("cached-token".into()))
    })
    .unwrap();
    assert_eq!(first.as_deref(), Some("cached-token"));
    let second = cached_keychain_read(Some((1, 100, 999)), &cache, || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some("different".into()))
    })
    .unwrap();
    assert_eq!(second.as_deref(), Some("cached-token"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Errors are never cached; changed fingerprint refreshes.
    let err_cache = Mutex::new(None);
    assert!(cached_keychain_read(Some((1, 100, 999)), &err_cache, || {
        Err(super::err(code::AUTH, "locked"))
    })
    .is_err());
    assert!(err_cache.lock().is_none());

    let third = cached_keychain_read(Some((1, 104, 1000)), &cache, || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(None)
    })
    .unwrap();
    assert_eq!(third, None);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[derive(Default)]
struct FakeCredentialStore {
    blob: Mutex<Option<Vec<u8>>>,
    writes: Mutex<usize>,
    /// Keeps something other than what was written.
    garbles_writes: bool,
    fail_read: bool,
    keeps_deleted: bool,
}

impl FakeCredentialStore {
    fn holding(blob: &[u8]) -> Self {
        Self {
            blob: Mutex::new(Some(blob.to_vec())),
            ..Self::default()
        }
    }
    fn blob(&self) -> Option<Vec<u8>> {
        self.blob.lock().clone()
    }
}

impl CredentialStore for FakeCredentialStore {
    fn read(&self) -> Result<Option<Vec<u8>>> {
        if self.fail_read {
            return Err(AppError::new(code::AUTH, "模拟凭据管理器读取失败"));
        }
        Ok(self.blob())
    }
    fn write(&self, blob: &[u8]) -> Result<()> {
        *self.writes.lock() += 1;
        *self.blob.lock() = Some(if self.garbles_writes {
            b"{}".to_vec()
        } else {
            blob.to_vec()
        });
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        if !self.keeps_deleted {
            *self.blob.lock() = None;
        }
        Ok(())
    }
}

#[test]
fn credential_manager_login_is_read_raw_or_prefixed_and_ahead_of_the_token_file() {
    let (_tmp, paths) = setup_paths();
    assert_eq!(
        read_windows_oauth(&paths, &FakeCredentialStore::default()).unwrap(),
        None
    );
    fs::write(paths.oauth_file(), FIRST_LOGIN).unwrap();
    assert_eq!(
        read_windows_oauth(&paths, &FakeCredentialStore::default())
            .unwrap()
            .as_deref(),
        Some(FIRST_LOGIN)
    );
    for stored in [
        SECOND_LOGIN.to_owned(),
        format!("{SECOND_LOGIN}\r\n"),
        encode_keyring(SECOND_LOGIN),
    ] {
        let store = FakeCredentialStore::holding(stored.as_bytes());
        assert_eq!(
            read_windows_oauth(&paths, &store).unwrap().as_deref(),
            Some(SECOND_LOGIN)
        );
    }
    // An entry holding no login does not hide the file Antigravity fell back to.
    for stored in [&b""[..], b" \n", b"go-keyring-base64:***"] {
        let store = FakeCredentialStore::holding(stored);
        assert_eq!(
            read_windows_oauth(&paths, &store).unwrap().as_deref(),
            Some(FIRST_LOGIN)
        );
    }
    let locked = FakeCredentialStore {
        fail_read: true,
        ..FakeCredentialStore::default()
    };
    assert!(read_windows_oauth(&paths, &locked).is_err());
}

#[test]
fn credential_manager_write_keeps_the_blob_limit_and_checks_what_was_stored() {
    let store = FakeCredentialStore::default();
    let fits = "x".repeat(CREDENTIAL_BLOB_LIMIT);
    set_credential_login(&store, Some(&fits)).unwrap();
    assert_eq!(store.blob().as_deref(), Some(fits.as_bytes()));
    // The limit counts bytes, as the credential manager does.
    let multibyte = "凭".repeat(CREDENTIAL_BLOB_LIMIT / 3 + 1);
    for oversized in [format!("{fits}x"), multibyte] {
        let error = set_credential_login(&store, Some(&oversized)).unwrap_err();
        assert_eq!(
            error.message,
            "Antigravity 登录凭据过大，无法写入 Windows 凭据管理器"
        );
    }
    assert_eq!(store.blob().as_deref(), Some(fits.as_bytes()));
    assert_eq!(*store.writes.lock(), 1);

    let garbling = FakeCredentialStore {
        garbles_writes: true,
        ..FakeCredentialStore::default()
    };
    assert!(set_credential_login(&garbling, Some(FIRST_LOGIN)).is_err());

    // Removing a login that is not there succeeds; one that stays is an error.
    set_credential_login(&FakeCredentialStore::default(), None).unwrap();
    set_credential_login(&store, None).unwrap();
    assert_eq!(store.blob(), None);
    let stuck = FakeCredentialStore {
        keeps_deleted: true,
        ..FakeCredentialStore::holding(FIRST_LOGIN.as_bytes())
    };
    assert!(set_credential_login(&stuck, None).is_err());
}

#[test]
fn windows_login_goes_to_the_credential_manager_or_falls_back_to_the_token_file() {
    let (_tmp, paths) = setup_paths();
    let store = FakeCredentialStore::default();
    write_windows_oauth(&paths, &store, FIRST_LOGIN).unwrap();
    assert_eq!(store.blob().as_deref(), Some(FIRST_LOGIN.as_bytes()));
    assert!(!paths.oauth_file().exists());

    // A login the credential manager cannot hold moves to the file, and the
    // entry that would be read ahead of it goes away.
    let oversized = format!(
        r#"{{"email":"u3@example.invalid","access_token":"{}"}}"#,
        "t".repeat(CREDENTIAL_BLOB_LIMIT)
    );
    write_windows_oauth(&paths, &store, &oversized).unwrap();
    assert_eq!(store.blob(), None);
    assert_eq!(fs::read_to_string(paths.oauth_file()).unwrap(), oversized);
    assert_eq!(
        read_windows_oauth(&paths, &store).unwrap().as_deref(),
        Some(oversized.as_str())
    );

    // A file left from then follows the next login, so it never differs
    // from the entry read ahead of it.
    write_windows_oauth(&paths, &store, SECOND_LOGIN).unwrap();
    assert_eq!(store.blob().as_deref(), Some(SECOND_LOGIN.as_bytes()));
    assert_eq!(
        fs::read_to_string(paths.oauth_file()).unwrap(),
        SECOND_LOGIN
    );

    let stuck = FakeCredentialStore {
        keeps_deleted: true,
        ..FakeCredentialStore::holding(FIRST_LOGIN.as_bytes())
    };
    assert!(write_windows_oauth(&paths, &stuck, &oversized).is_err());
}

#[test]
fn fixture_homes_never_reach_the_keychain() {
    let (_tmp, paths) = setup_paths();
    assert!(!paths.system_store());
    assert!(read_oauth(&paths, false).unwrap().is_none());
    write_oauth(&paths, r#"{"access_token":"fixture"}"#).unwrap();
    assert_eq!(
        fs::read_to_string(paths.oauth_file()).unwrap(),
        r#"{"access_token":"fixture"}"#
    );
}

#[test]
fn captured_keychain_login_never_falls_back_to_the_legacy_file() {
    let (_tmp, paths) = setup_paths();
    let legacy = r#"{"auth_method":"google-oauth","id_token_email":"old@example.invalid","token":{"access_token":"fixture-old","refresh_token":"fixture-old"}}"#;
    fs::write(paths.oauth_file(), legacy).unwrap();
    for value in ["", "go-keyring-base64:", "go-keyring-base64:***"] {
        let (state, clean) = state_with_oauth(&paths, decode_keyring(value)).unwrap();
        assert!(clean);
        assert!(!state.oauth_present);
        assert!(state.accounts.is_empty());
        assert_eq!(fs::read_to_string(paths.oauth_file()).unwrap(), legacy);
    }
    let current = legacy
        .replace("old@example.invalid", "new@example.invalid")
        .replace("fixture-old", "fixture-new");
    let (state, _) = state_with_oauth(&paths, decode_keyring(&encode_keyring(&current))).unwrap();
    assert_eq!(state.oauth_email.as_deref(), Some("new@example.invalid"));
    let before = load_vault(&paths).unwrap().0.activations;
    let refreshed = current.replace("fixture-new", "fixture-refreshed");
    state_with_oauth(&paths, Some(refreshed)).unwrap();
    assert_eq!(load_vault(&paths).unwrap().0.activations, before);
    let (logged_out, _) = state_with_oauth(&paths, None).unwrap();
    assert!(!logged_out.oauth_present);
    assert!(logged_out.accounts.iter().all(|a| !a.current));
    assert_eq!(logged_out.accounts.len(), 1);
    assert_eq!(fs::read_to_string(paths.oauth_file()).unwrap(), legacy);
}

#[test]
fn credential_read_failure_keeps_the_saved_account_and_can_be_retried() {
    let (_tmp, paths) = setup_paths();
    let raw = br#"{"auth_method":"google-oauth","user":{"email":"fixture@example.invalid"},"token":{"access_token":"fixture"}}"#;
    fs::write(paths.oauth_file(), raw).unwrap();
    let before = state_at(&paths).unwrap();
    let vault = fs::read(paths.vault_file()).unwrap();
    fs::remove_file(paths.oauth_file()).unwrap();
    fs::create_dir(paths.oauth_file()).unwrap();
    assert!(state_at(&paths).is_err());
    assert_eq!(fs::read(paths.vault_file()).unwrap(), vault);
    fs::remove_dir(paths.oauth_file()).unwrap();
    fs::write(paths.oauth_file(), raw).unwrap();
    let after = state_at(&paths).unwrap();
    assert_eq!(after.accounts.len(), before.accounts.len());
    assert_eq!(after.accounts[0].id, before.accounts[0].id);
    assert!(after.accounts[0].current);
}

#[test]
fn new_oauth_notice_survives_hidden_updates_and_is_drained_once() {
    let (_tmp, paths) = setup_paths();
    let state = state_with_oauth(&paths, Some(serde_json::json!({"auth_method":"google-oauth","user":{"email":"fixture@example.invalid"},"token":{"access_token":"fixture","refresh_token":"fixture"}}).to_string())).unwrap().0;
    assert!(state.oauth_added);
    let pending = crate::PendingMainEvents::default();
    pending.record_antigravity_auth(&state);
    let mut next = state.clone();
    next.oauth_added = false;
    next.revision = "newer".into();
    pending.record_antigravity_auth(&next);
    assert!(pending.drain_for_window("quick").is_none());
    let drained = pending.drain_for_window("main").unwrap();
    assert!(drained.antigravity_auth.as_ref().unwrap().oauth_added);
    assert_eq!(drained.antigravity_auth.unwrap().revision, "newer");
    assert!(pending.drain().antigravity_auth.is_none());
}

/// The vault an earlier version left behind: one account of each kind it
/// supported, plus what it recorded while a Vertex API Key was in use.
fn write_retired_vault(paths: &Paths, gateway: Value) {
    let vault = serde_json::json!({
        "schema": 1,
        "entries": [
            {
                "id": "fixture-oauth",
                "name": "Fixture OAuth",
                "kind": "oauth",
                "sourceType": "google-oauth",
                "email": "fixture@example.invalid",
                "projectId": null,
                "payload": "{\"email\":\"fixture@example.invalid\",\"access_token\":\"fixture\"}",
                "updatedAt": 10
            },
            {
                "id": "fixture-adc",
                "name": "Fixture ADC",
                "kind": "adc",
                "sourceType": "authorized_user",
                "email": null,
                "projectId": "fixture-project",
                "payload": "{\"type\":\"authorized_user\",\"refresh_token\":\"fixture\"}",
                "updatedAt": 20
            },
            {
                "id": "fixture-key",
                "name": "Fixture Vertex",
                "kind": "vertex_key",
                "sourceType": "vertex_key",
                "email": null,
                "projectId": null,
                "payload": "{\"apiKey\":\"fixture-vertex-key\",\"projectId\":null,\"location\":\"global\",\"models\":[\"gemini-2.5-flash\"]}",
                "updatedAt": 30
            }
        ],
        "observedRevisions": ["fixture-revision"],
        "activeOauthId": "fixture-oauth",
        "activations": [
            { "accountId": "fixture-oauth", "sinceMs": 1_000 },
            { "accountId": "fixture-adc", "sinceMs": 2_000 },
            { "accountId": "fixture-key", "sinceMs": 3_000 }
        ],
        "gateway": gateway
    });
    fs::write(
        paths.vault_file(),
        serde_json::to_vec_pretty(&vault).unwrap(),
    )
    .unwrap();
}
/// What that version recorded for the key in use, with the app settings the
/// key replaced.
fn key_takeover(previous: Value) -> Value {
    serde_json::json!({
        "accountId": "fixture-key",
        "token": "fixture-local-token",
        "port": 43721,
        "previous": previous
    })
}
/// The app settings as that version left them for the key.
fn managed_app_settings() -> Value {
    serde_json::json!({
        "gateway": {
            "enabled": true,
            "url": "http://127.0.0.1:43721/antigravity",
            "apiKey": "fixture-local-token",
            "wireProtocol": "genai",
            "customHeaders": {}
        },
        "models": {
            "defaultModelId": "gemini-2.5-flash",
            "list": [{ "modelId": "gemini-2.5-flash" }]
        },
        "theme": "fixture-theme"
    })
}
fn app_settings_file(paths: &Paths) -> PathBuf {
    let home = paths.gemini_home.parent().unwrap();
    home.join(".antigravity/settings.json")
}
fn write_app_settings(paths: &Paths, text: &str) {
    let file = app_settings_file(paths);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}
fn saved_vault(paths: &Paths) -> Value {
    serde_json::from_slice(&fs::read(paths.vault_file()).unwrap()).unwrap()
}

#[test]
fn an_old_vault_loads_with_only_its_oauth_accounts() {
    let (_tmp, paths) = setup_paths();
    let previous = serde_json::json!({ "gateway": null, "models": null, "fileExisted": false });
    write_retired_vault(&paths, key_takeover(previous));
    let written = fs::read(paths.vault_file()).unwrap();

    let (vault, _) = load_vault(&paths).unwrap();
    assert_eq!(vault.entries.len(), 1);
    assert_eq!(vault.entries[0].id, "fixture-oauth");
    assert_eq!(vault.active_oauth_id.as_deref(), Some("fixture-oauth"));
    assert_eq!(vault.observed_revisions, ["fixture-revision"]);
    let state = state_at(&paths).unwrap();
    assert_eq!(state.accounts.len(), 1);
    assert_eq!(state.accounts[0].id, "fixture-oauth");

    // Usage recorded under a retired account stays attributed to its id.
    let timeline = provider_timeline(&paths.data_dir, Some(&paths.gemini_home));
    for (at, id) in [
        (1_500, "fixture-oauth"),
        (2_500, "fixture-adc"),
        (3_500, "fixture-key"),
    ] {
        assert_eq!(timeline.provider_at(at), Some(id));
    }
    // Reading leaves the file as it is; only the migration rewrites it.
    assert_eq!(fs::read(paths.vault_file()).unwrap(), written);
}

#[test]
fn migration_hands_back_app_settings_a_key_took_over_and_drops_retired_accounts() {
    let (_tmp, paths) = setup_paths();
    let home = paths.gemini_home.parent().unwrap();
    // What the retired account kinds used stays exactly as it is.
    let untouched = [
        (
            home.join(".config/gcloud/application_default_credentials.json"),
            r#"{"type":"authorized_user","refresh_token":"fixture"}"#,
        ),
        (
            home.join(".config/agswitch/adc.json"),
            r#"{"type":"service_account"}"#,
        ),
        (
            paths.gemini_home.join("config/config.json"),
            r#"{"userSettings":{"enableAdc":true}}"#,
        ),
    ];
    for (file, text) in &untouched {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    let previous = serde_json::json!({
        "gateway": { "enabled": false, "customHeaders": { "fixture": "kept" }, "fixtureLimit": 3 },
        "models": { "defaultModelId": "fixture-original" },
        "fileExisted": true
    });
    write_retired_vault(&paths, key_takeover(previous.clone()));
    write_app_settings(&paths, &managed_app_settings().to_string());

    assert!(migrate_retired_accounts(&paths).unwrap());
    let settings: Value =
        serde_json::from_slice(&fs::read(app_settings_file(&paths)).unwrap()).unwrap();
    assert_eq!(
        settings,
        serde_json::json!({
            "gateway": previous["gateway"],
            "models": previous["models"],
            "theme": "fixture-theme"
        })
    );
    let saved = saved_vault(&paths);
    assert!(saved.get("gateway").is_none());
    assert_eq!(saved["entries"].as_array().unwrap().len(), 1);
    assert_eq!(saved["entries"][0]["id"], "fixture-oauth");
    assert_eq!(saved["entries"][0]["kind"], "oauth");
    assert!(saved["entries"][0].get("projectId").is_none());
    assert_eq!(saved["activeOauthId"], "fixture-oauth");
    assert_eq!(
        saved["observedRevisions"],
        serde_json::json!(["fixture-revision"])
    );
    assert_eq!(saved["activations"].as_array().unwrap().len(), 3);
    for (file, text) in &untouched {
        assert_eq!(fs::read_to_string(file).unwrap(), *text);
    }

    // A migrated vault is left untouched.
    let files = [paths.vault_file(), app_settings_file(&paths)];
    let migrated = files.clone().map(|file| fs::read(file).unwrap());
    assert!(!migrate_retired_accounts(&paths).unwrap());
    assert_eq!(files.map(|file| fs::read(file).unwrap()), migrated);
}

#[test]
fn migration_removes_app_settings_that_only_held_the_takeover() {
    let (_tmp, paths) = setup_paths();
    let previous = serde_json::json!({ "gateway": null, "models": null, "fileExisted": false });
    write_retired_vault(&paths, key_takeover(previous));
    let mut settings = managed_app_settings();
    settings.as_object_mut().unwrap().remove("theme");
    write_app_settings(&paths, &settings.to_string());

    assert!(migrate_retired_accounts(&paths).unwrap());
    assert!(!app_settings_file(&paths).exists());
    assert!(saved_vault(&paths).get("gateway").is_none());
}

#[test]
fn migration_leaves_app_settings_changed_since_the_takeover() {
    let previous = serde_json::json!({
        "gateway": { "enabled": false },
        "models": null,
        "fileExisted": true
    });
    let changed = |pointer: &str, value: Value| {
        let mut settings = managed_app_settings();
        *settings.pointer_mut(pointer).unwrap() = value;
        Some(settings.to_string())
    };
    for settings in [
        changed("/gateway/url", "https://example.invalid/v1".into()),
        changed("/gateway/apiKey", "fixture-own-key".into()),
        changed("/gateway/enabled", false.into()),
        Some("{ not json".to_owned()),
        None,
    ] {
        let (_tmp, paths) = setup_paths();
        write_retired_vault(&paths, key_takeover(previous.clone()));
        if let Some(text) = &settings {
            write_app_settings(&paths, text);
        }
        assert!(migrate_retired_accounts(&paths).unwrap());
        assert_eq!(fs::read_to_string(app_settings_file(&paths)).ok(), settings);
        // The retired accounts and the takeover record go all the same.
        let saved = saved_vault(&paths);
        assert!(saved.get("gateway").is_none());
        assert_eq!(saved["entries"].as_array().unwrap().len(), 1);
        assert!(!migrate_retired_accounts(&paths).unwrap());
    }

    // Without a recorded takeover the app settings are not Vela's to restore.
    let (_tmp, paths) = setup_paths();
    write_retired_vault(&paths, Value::Null);
    let managed = managed_app_settings().to_string();
    write_app_settings(&paths, &managed);
    assert!(migrate_retired_accounts(&paths).unwrap());
    assert_eq!(
        fs::read_to_string(app_settings_file(&paths)).unwrap(),
        managed
    );
    assert_eq!(saved_vault(&paths)["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn migration_skips_a_vault_without_retired_content() {
    let (_tmp, paths) = setup_paths();
    assert!(!migrate_retired_accounts(&paths).unwrap());
    assert!(!paths.vault_file().exists());

    // OAuth accounts as an earlier version wrote them, and a file that
    // cannot be read, both stay byte for byte.
    let oauth_only = serde_json::json!({
        "schema": 1,
        "entries": [{
            "id": "fixture-oauth",
            "name": "Fixture OAuth",
            "kind": "oauth",
            "sourceType": "google-oauth",
            "email": "fixture@example.invalid",
            "projectId": null,
            "payload": "{\"access_token\":\"fixture\"}",
            "updatedAt": 10
        }],
        "observedRevisions": [],
        "activeOauthId": "fixture-oauth",
        "activations": [],
        "gateway": null
    });
    for vault in [oauth_only.to_string(), "{ not json".to_owned()] {
        fs::write(paths.vault_file(), &vault).unwrap();
        assert!(!migrate_retired_accounts(&paths).unwrap());
        assert_eq!(fs::read_to_string(paths.vault_file()).unwrap(), vault);
    }
}
