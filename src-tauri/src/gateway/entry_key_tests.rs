//! Handing a key out and taking it back as edits of the gateway; every name,
//! address and token here is a fixture.
use super::*;
use crate::gateway::test_support::{form, hand_out_key};

fn save(id: Option<&str>, name: &str) -> Edit {
    Edit::SaveEntryKey {
        id: id.map(str::to_owned),
        name: name.into(),
    }
}

fn names(view: &View) -> Vec<&str> {
    view.entry_keys
        .iter()
        .map(|key| key.name.as_str())
        .collect()
}

/// Whether the shared listener lets `key` in for this gateway's client.
fn routed(g: &Gateway, key: &str) -> bool {
    let mut headers = hyper::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {key}").parse().unwrap());
    let uri: hyper::Uri = "/v1/responses".parse().unwrap();
    match listener::route(&uri, &headers, &g.0.registry) {
        Ok((client, _)) => {
            assert_eq!(client, g.0.client);
            true
        }
        Err(listener::RouteError::Unauthorized) => false,
        Err(listener::RouteError::NotFound) => panic!("fixture path should be a gateway's"),
    }
}

fn stored(t: &tempfile::TempDir) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(t.path().join("gateway.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn a_key_is_made_by_the_gateway_named_once_and_kept_through_a_rename() {
    let (t, g) = fixture(vec!["https://a.invalid".into()]).await;
    let own = g.0.inner.lock().store.local_token.clone();
    assert!(g.view().entry_keys.is_empty());
    assert!(
        stored(&t).get("entryKeys").is_none(),
        "a gateway that handed nothing out is stored as it was before keys"
    );

    // The name is kept without the blanks around it.
    let view = g
        .edit(
            save(None, "  fixture-partner  "),
            &g.view().revision,
            t.path(),
        )
        .unwrap();
    assert_eq!(names(&view), ["fixture-partner"]);
    let id = view.entry_keys[0].id.clone();
    let key = g.entry_key_token(&id).unwrap();
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    assert_eq!(key.len(), 67);
    assert!(key.starts_with("gs_"));
    assert!(key[3..].bytes().all(|b| b.is_ascii_hexdigit()));
    assert_ne!(key, own);
    assert!(routed(&g, &key) && routed(&g, &own));
    // Only its id and name leave Rust in a view.
    let view = serde_json::to_string(&g.view()).unwrap();
    assert!(view.contains(&id) && !view.contains(&key) && !view.contains(&own));
    assert_eq!(
        stored(&t)["entryKeys"],
        serde_json::json!([{"id": id, "name": "fixture-partner", "token": key}])
    );

    // A name is one to 120 visible characters, the client's own key keeps
    // its, and no two keys of a client share one.
    let long = "k".repeat(121);
    for refused in [
        "",
        "   ",
        "fixture\nbroken",
        long.as_str(),
        model::OWN_KEY_NAME,
    ] {
        let before = g.view().revision;
        let error = g
            .edit(save(None, refused), &before, t.path())
            .err()
            .unwrap();
        assert_eq!(error.code, "NAME", "{refused:?}");
        assert_eq!(g.view().revision, before);
    }
    let (other, other_key) = hand_out_key(&g, t.path(), &"k".repeat(120));
    assert_ne!(other, id);
    assert_ne!(other_key, key);
    for (target, taken) in [
        (None, "fixture-partner"),
        (Some(other.as_str()), "fixture-partner"),
    ] {
        let error = g
            .edit(save(target, taken), &g.view().revision, t.path())
            .err()
            .unwrap();
        assert_eq!(error.code, "NAME", "{target:?}");
    }

    // A rename changes the name alone: the id and the key go on as they were.
    let before = g.view().revision;
    let view = g
        .edit(save(Some(&id), "fixture-renamed"), &before, t.path())
        .unwrap();
    assert_ne!(view.revision, before);
    assert_eq!(view.entry_keys[0].id, id);
    assert_eq!(view.entry_keys[0].name, "fixture-renamed");
    assert_eq!(g.entry_key_token(&id).unwrap(), key);
    assert!(routed(&g, &key));
    // Its own name is free for it to keep.
    g.edit(
        save(Some(&id), "fixture-renamed"),
        &g.view().revision,
        t.path(),
    )
    .unwrap();

    // What is not there can be neither renamed, taken back nor read.
    let missing = uuid::Uuid::new_v4().to_string();
    for edit in [
        save(Some(&missing), "fixture-nobody"),
        Edit::DeleteEntryKey {
            id: missing.clone(),
        },
    ] {
        let before = g.view().revision;
        assert_eq!(
            g.edit(edit, &before, t.path()).err().unwrap().code,
            "MISSING"
        );
        assert_eq!(g.view().revision, before);
    }
    assert_eq!(g.entry_key_token(&missing).err().unwrap().code, "MISSING");

    // Taken back, a key is gone from the view, the file and the listener,
    // and the other one is as it was.
    let view = g
        .edit(
            Edit::DeleteEntryKey { id: id.clone() },
            &g.view().revision,
            t.path(),
        )
        .unwrap();
    assert_eq!(view.entry_keys.len(), 1);
    assert_eq!(view.entry_keys[0].id, other);
    assert!(!routed(&g, &key));
    assert!(routed(&g, &other_key) && routed(&g, &own));
    assert_eq!(g.entry_key_token(&id).err().unwrap().code, "MISSING");
    assert!(!std::fs::read_to_string(t.path().join("gateway.json"))
        .unwrap()
        .contains(&key));
    // A name is free again once its key is gone.
    hand_out_key(&g, t.path(), "fixture-renamed");

    // A gateway opened over the same data hands the same keys out.
    drop(g);
    let reopened = Gateway::new(t.path().to_path_buf()).unwrap();
    assert_eq!(reopened.entry_key_token(&other).unwrap(), other_key);
    assert!(routed(&reopened, &other_key));
    assert!(!routed(&reopened, &key));
}

#[tokio::test]
async fn the_later_of_two_editors_of_a_key_is_told_of_the_conflict() {
    let (t, g) = fixture(vec!["https://a.invalid".into()]).await;
    for running in [false, true] {
        if running {
            start(&g, &t).await;
        }
        let name = |what: &str| format!("fixture-{what}-{running}");
        // Two windows read the same state; the first one to save wins.
        let seen = g.view().revision;
        let first = g.edit(save(None, &name("first")), &seen, t.path()).unwrap();
        assert_ne!(first.revision, seen);
        let id = first
            .entry_keys
            .iter()
            .find(|key| key.name == name("first"))
            .unwrap()
            .id
            .clone();
        for late in [
            save(None, &name("second")),
            save(Some(&id), &name("renamed")),
            Edit::DeleteEntryKey { id: id.clone() },
        ] {
            let error = g.edit(late, &seen, t.path()).err().unwrap();
            assert_eq!(error.code, "CONFLICT");
            assert_eq!(g.view().revision, first.revision);
        }
        // Each of the three is an edit like any other: it moves the revision
        // an editor of anything else in the gateway has to hold.
        let renamed = g
            .edit(save(Some(&id), &name("renamed")), &first.revision, t.path())
            .unwrap();
        assert_ne!(renamed.revision, first.revision);
        let provider = renamed.providers[0].id.clone();
        let stale = g
            .edit(
                Edit::RenameProvider {
                    id: provider,
                    name: name("provider"),
                },
                &first.revision,
                t.path(),
            )
            .err()
            .unwrap();
        assert_eq!(stale.code, "CONFLICT");
        let deleted = g
            .edit(Edit::DeleteEntryKey { id }, &renamed.revision, t.path())
            .unwrap();
        assert_ne!(deleted.revision, renamed.revision);
        assert!(!names(&deleted).contains(&name("renamed").as_str()));
    }
    g.stop().await.unwrap();
}

#[tokio::test]
async fn a_running_gateway_hands_out_and_takes_back_a_key_whatever_became_of_the_clients_config() {
    let (t, g) = fixture(vec!["https://a.invalid".into()]).await;
    start(&g, &t).await;
    let config = t.path().join("config.toml");
    let journal = t.path().join("gateway-recovery.json");
    let live = std::fs::read_to_string(&config).unwrap();
    let (before, before_key) = hand_out_key(&g, t.path(), "fixture-before");

    // Something else rewrites the connection the gateway manages.
    let port = g.view().settings.port;
    let managed = format!("http://127.0.0.1:{port}/v1");
    assert!(live.contains(&managed));
    let external = live.replace(&managed, "https://elsewhere.invalid/v1");
    std::fs::write(&config, &external).unwrap();
    let record = std::fs::read(&journal).unwrap();

    // An edit that the client's file has to follow is refused...
    let provider = g.view().providers[0].id.clone();
    let revision = g.view().revision;
    let refused = g
        .edit(
            Edit::RenameProvider {
                id: provider.clone(),
                name: "fixture-refused".into(),
            },
            &revision,
            t.path(),
        )
        .err()
        .unwrap();
    assert_eq!(refused.code, "CONFLICT");
    assert_eq!(g.view().revision, revision);

    // ...and a key, of which that file says nothing, is not held up by it.
    let (added, added_key) = hand_out_key(&g, t.path(), "fixture-added");
    assert!(routed(&g, &added_key));
    g.edit(
        save(Some(&added), "fixture-renamed"),
        &g.view().revision,
        t.path(),
    )
    .unwrap();
    for id in [before, added] {
        g.edit(Edit::DeleteEntryKey { id }, &g.view().revision, t.path())
            .unwrap();
    }
    assert!(!routed(&g, &before_key) && !routed(&g, &added_key));
    assert!(g.view().entry_keys.is_empty());
    assert!(stored(&t).get("entryKeys").is_none());
    assert_ne!(g.view().revision, revision);
    // Neither the client's file nor the transaction record was touched.
    assert_eq!(std::fs::read_to_string(&config).unwrap(), external);
    assert_eq!(std::fs::read(&journal).unwrap(), record);
    assert!(g.view().running);

    // With the file as the gateway left it, the other edit goes through on
    // top of what the keys did to the store, and the gateway stops cleanly.
    std::fs::write(&config, &live).unwrap();
    g.edit(
        Edit::RenameProvider {
            id: provider,
            name: "fixture-accepted".into(),
        },
        &g.view().revision,
        t.path(),
    )
    .unwrap();
    g.stop().await.unwrap();
    assert!(!journal.exists());
    assert_eq!(g.view().providers[0].name, "fixture-accepted");
}

#[tokio::test]
async fn a_key_waits_for_a_store_the_transaction_record_has_yet_to_write() {
    let (t, g) = fixture(vec!["https://a.invalid".into()]).await;
    start(&g, &t).await;
    let (id, key) = hand_out_key(&g, t.path(), "fixture-held");
    let journal = t.path().join("gateway-recovery.json");
    let store = t.path().join("gateway.json");
    let settled = std::fs::read(&journal).unwrap();
    let kept = std::fs::read(&store).unwrap();

    // A store that was recorded and never reached the file: it knows nothing
    // of a key written past it, and finishing it would take the key away.
    let mut record: serde_json::Value = serde_json::from_slice(&settled).unwrap();
    assert!(record["store_after"].is_null());
    record["store_before"] = serde_json::json!(String::from_utf8(kept.clone()).unwrap());
    record["store_after"] = serde_json::json!(String::from_utf8(kept.clone()).unwrap());
    let pending = serde_json::to_vec(&record).unwrap();
    std::fs::write(&journal, &pending).unwrap();

    for edit in [
        save(None, "fixture-late"),
        save(Some(&id), "fixture-renamed"),
        Edit::DeleteEntryKey { id: id.clone() },
    ] {
        let revision = g.view().revision;
        let error = g.edit(edit, &revision, t.path()).err().unwrap();
        assert_eq!(error.code, "RECOVERY");
        assert_eq!(g.view().revision, revision);
    }
    assert_eq!(names(&g.view()), ["fixture-held"]);
    assert!(routed(&g, &key));
    assert_eq!(std::fs::read(&store).unwrap(), kept);
    assert_eq!(std::fs::read(&journal).unwrap(), pending);

    // Once the record holds no store, the key can be taken back.
    std::fs::write(&journal, &settled).unwrap();
    g.edit(Edit::DeleteEntryKey { id }, &g.view().revision, t.path())
        .unwrap();
    assert!(!routed(&g, &key));
    g.stop().await.unwrap();

    // Stopped, an unfinished transaction holds a key up as it does any edit.
    std::fs::write(&journal, &settled).unwrap();
    let error = g
        .edit(save(None, "fixture-stopped"), &g.view().revision, t.path())
        .err()
        .unwrap();
    assert_eq!(error.code, "RECOVERY");
    std::fs::remove_file(&journal).unwrap();
    hand_out_key(&g, t.path(), "fixture-stopped");
}

#[tokio::test]
async fn every_client_hands_out_its_own_keys() {
    let root = tempfile::tempdir().unwrap();
    let codex = Gateway::new(root.path().join("codex")).unwrap();
    let claude = codex.companion(root.path().join("claude")).unwrap();
    let grok = codex.grok_companion(root.path().join("grok")).unwrap();
    let gateways = [&codex, &claude, &grok];
    let mut keys = Vec::new();
    for g in gateways {
        // The same name under each of them: a name is a client's own.
        let (id, key) = hand_out_key(g, root.path(), "fixture-shared-name");
        keys.push((id, key));
    }
    for (index, g) in gateways.into_iter().enumerate() {
        for (other, (id, key)) in keys.iter().enumerate() {
            assert_eq!(routed_to(g, key), Some(gateways[other].0.client));
            assert_eq!(g.entry_key_token(id).is_ok(), index == other);
        }
    }
    // A provider added to one client leaves every key where it was.
    codex
        .edit(
            form(serde_json::json!({"baseUrl": "https://a.invalid", "token": "fixture"})),
            &codex.view().revision,
            root.path(),
        )
        .unwrap();
    for (g, (_, key)) in gateways.into_iter().zip(&keys) {
        assert_eq!(routed_to(g, key), Some(g.0.client));
    }
}

/// The client the shared listener sends `key` to.
fn routed_to(g: &Gateway, key: &str) -> Option<ClientId> {
    let mut headers = hyper::HeaderMap::new();
    headers.insert("x-api-key", key.parse().unwrap());
    let uri: hyper::Uri = "/v1/responses".parse().unwrap();
    listener::route(&uri, &headers, &g.0.registry)
        .ok()
        .map(|(client, _)| client)
}
