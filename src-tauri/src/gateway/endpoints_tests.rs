use super::super::{
    model::{self, Edit, Provider, Store},
    replay, Gateway,
};
use super::{Batch, Measurement, Service};
use crate::gateway::test_support::form;
use crate::gateway::test_support::loopback::serve;
use crate::gateway::test_support::provider_at;
use http_body_util::BodyExt;
use hyper::{header, Response};
use parking_lot::Mutex;
use std::{sync::Arc, time::Instant};

#[test]
fn store_normalizes_validates_and_swaps_endpoint_roles_without_rotating_provider_identity() {
    let mut store = Store::default();
    let primary_raw = "HTTPS://PRIMARY.example.invalid:443/v1/";
    store
        .edit(
            form(serde_json::json!({
                "name": "fixture",
                "baseUrl": primary_raw,
                "token": "fixture-token",
            })),
            false,
        )
        .unwrap();
    let provider = &store.providers[0];
    let id = provider.id.clone();
    let token = provider.token.clone();
    let version = provider.version.clone();
    let primary = model::base_url(primary_raw, store.settings.port)
        .unwrap()
        .to_string()
        .trim_end_matches('/')
        .to_owned();
    let secondary = "https://secondary.example.invalid/v1".to_owned();

    store
        .edit(
            Edit::EndpointsProvider {
                id: id.clone(),
                base_urls: vec![
                    " https://SECONDARY.example.invalid/v1/// ".into(),
                    primary_raw.into(),
                    secondary.clone(),
                ],
            },
            false,
        )
        .unwrap();
    assert_eq!(store.provider_endpoints[&id], vec![secondary.clone()]);
    for invalid in [
        "ftp://secondary.example.invalid/v1",
        "https://user:pass@secondary.example.invalid/v1",
        "https://secondary.example.invalid/v1?fixture=1",
        "http://localhost:43721/v1",
    ] {
        assert!(
            model::endpoint_urls(
                "https://primary.example.invalid/v1",
                &[invalid.into()],
                store.settings.port
            )
            .is_err(),
            "invalid endpoint accepted: {invalid}"
        );
    }

    store
        .edit(
            Edit::SwapEndpoint {
                id: id.clone(),
                base_url: secondary.clone(),
            },
            false,
        )
        .unwrap();
    assert_eq!(store.providers[0].base_url, secondary);
    assert_eq!(store.provider_endpoints[&id], vec![primary.clone()]);
    assert_eq!(
        (
            store.providers[0].id.clone(),
            store.providers[0].token.clone(),
            store.providers[0].version.clone()
        ),
        (id.clone(), token.clone(), version.clone())
    );

    store
        .edit(
            Edit::SwapEndpoint {
                id: id.clone(),
                base_url: primary.clone(),
            },
            false,
        )
        .unwrap();
    assert_eq!(store.providers[0].base_url, primary);
    assert_eq!(
        store.provider_endpoints[&id],
        vec!["https://secondary.example.invalid/v1"]
    );
    assert_eq!(
        (
            store.providers[0].id.clone(),
            store.providers[0].token.clone(),
            store.providers[0].version.clone()
        ),
        (id, token, version)
    );
}

#[test]
fn gateway_rejects_stale_edit_revision_and_persists_endpoints_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(dir.path().to_path_buf()).unwrap();
    gateway
        .edit(
            form(serde_json::json!({
                "name": "fixture",
                "baseUrl": "https://primary.example.invalid/v1",
                "token": "fixture-token",
            })),
            &gateway.view().revision,
            dir.path(),
        )
        .unwrap();
    let provider = gateway.0.inner.lock().store.providers[0].clone();
    let stale = gateway.view().revision;
    gateway
        .edit(
            Edit::EndpointsProvider {
                id: provider.id.clone(),
                base_urls: vec!["https://secondary.example.invalid/v1///".into()],
            },
            &stale,
            dir.path(),
        )
        .unwrap();
    let error = gateway
        .edit(
            Edit::RenameProvider {
                id: provider.id.clone(),
                name: "stale-edit".into(),
            },
            &stale,
            dir.path(),
        )
        .err()
        .unwrap();
    assert_eq!(error.code, "CONFLICT");
    let before_restart = gateway.0.inner.lock().store.clone();
    drop(gateway);

    let restarted = Gateway::new(dir.path().to_path_buf()).unwrap();
    let after_restart = restarted.0.inner.lock().store.clone();
    assert_eq!(after_restart.providers.len(), 1);
    assert_eq!(after_restart.providers[0].id, provider.id);
    assert_eq!(after_restart.providers[0].token, provider.token);
    assert_eq!(after_restart.providers[0].version, provider.version);
    assert_eq!(
        after_restart.providers[0].name,
        before_restart.providers[0].name
    );
    assert_eq!(
        after_restart.provider_endpoints,
        before_restart.provider_endpoints
    );
}

#[test]
fn service_prefers_fastest_endpoint_and_keeps_thread_and_response_pins_after_swap() {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(dir.path().join("endpoint-owners.json"));
    let primary_raw = "HTTPS://PRIMARY.example.invalid:443/v1/";
    let primary = model::base_url(primary_raw, 15722)
        .unwrap()
        .to_string()
        .trim_end_matches('/')
        .to_owned();
    let secondary = "https://secondary.example.invalid/v1".to_owned();
    let provider = provider_at(primary_raw);
    let urls = vec![primary_raw.to_owned(), secondary.clone()];
    let config = Service::config(&provider, &urls);
    service.samples.lock().insert(
        provider.id.clone(),
        Batch {
            config,
            checked: Instant::now(),
            rows: vec![
                Measurement {
                    base_url: primary_raw.to_owned(),
                    latency_ms: Some(12),
                    status: Some(200),
                    error: None,
                    checked_at: 1,
                },
                Measurement {
                    base_url: secondary.clone(),
                    latency_ms: Some(80),
                    status: Some(200),
                    error: None,
                    checked_at: 1,
                },
            ],
        },
    );
    assert_eq!(service.preferred(&provider, &urls), primary_raw);
    let thread = "fixture-thread";
    let response = "fixture-response";
    assert_eq!(
        service
            .select(&provider, &urls, Some(thread), None, true)
            .unwrap(),
        primary_raw
    );
    service.remember(&provider, response);

    let swapped = Provider {
        base_url: secondary.clone(),
        ..provider.clone()
    };
    let swapped_urls = vec![secondary, primary.clone()];
    assert_eq!(
        service
            .select(&swapped, &swapped_urls, Some(thread), None, true)
            .unwrap(),
        primary
    );
    assert_eq!(
        service
            .select(&swapped, &swapped_urls, None, Some(response), false)
            .unwrap(),
        primary
    );

    let restarted = Service::new(dir.path().join("endpoint-owners.json"));
    assert_eq!(
        restarted
            .select(&swapped, &swapped_urls, Some(thread), None, true)
            .unwrap(),
        primary
    );
}

async fn loopback_server(seen: Arc<Mutex<Vec<(String, bool)>>>) -> u16 {
    serve(move |request| {
        let seen = seen.clone();
        async move {
            let (parts, body) = request.into_parts();
            let _ = body.collect().await;
            seen.lock().push((
                parts.uri.path().to_owned(),
                parts.headers.contains_key(header::AUTHORIZATION),
            ));
            Response::builder()
                .status(204)
                .body(replay::empty())
                .unwrap()
        }
    })
    .await
}

#[tokio::test]
async fn loopback_probe_sends_no_authorization_or_business_rpm_and_keeps_edit_revision() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let upstream_port = loopback_server(seen.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(dir.path().to_path_buf()).unwrap();
    gateway
        .edit(
            form(serde_json::json!({
                "name": "loopback",
                "baseUrl": format!("http://127.0.0.1:{upstream_port}/v1"),
                "token": "fixture-token",
            })),
            &gateway.view().revision,
            dir.path(),
        )
        .unwrap();
    let provider_id = gateway.view().providers[0].id.clone();
    gateway
        .edit(
            Edit::RpmProvider {
                id: provider_id.clone(),
                max_rpm: 7,
            },
            &gateway.view().revision,
            dir.path(),
        )
        .unwrap();
    let before = gateway.view();
    let _ = gateway.probe_endpoints(&provider_id, true).await.unwrap();
    let after = gateway.view();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.providers[0].rpm_used, 0);
    let requests = seen.lock().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|(path, has_auth)| path == "/v1" && !has_auth));
}
