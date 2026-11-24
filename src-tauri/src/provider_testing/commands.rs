use super::*;
use crate::Runtime;
pub fn provider_test_state(r: &Runtime, client_id: ClientId, provider_id: String) -> Result<State> {
    r.provider_tests.state(client_id, &provider_id)
}
pub async fn provider_test_start(r: &Runtime, request: Request) -> Result<Job> {
    let service = r.provider_tests.clone();
    tokio::task::spawn_blocking(move || service.start(request))
        .await
        .map_err(|_| failure("测试资源加载失败"))?
}
pub fn provider_test_save_prompt(r: &Runtime, prompt: String) -> Result<()> {
    r.provider_tests.set_prompt(prompt)
}
pub fn provider_test_set_platform(
    r: &Runtime,
    client_id: ClientId,
    platform: PlatformSettings,
) -> Result<()> {
    r.provider_tests.set_platform(client_id, platform)
}
pub fn provider_test_query(r: &Runtime, id: String) -> Result<Detail> {
    r.provider_tests.detail(&id)
}
pub fn provider_test_cancel(r: &Runtime, id: String) -> Result<()> {
    r.provider_tests.cancel(&id)
}
pub fn provider_test_retry(r: &Runtime, id: String) -> Result<Job> {
    r.provider_tests.retry(&id)
}
pub fn provider_test_set_detection(
    r: &Runtime,
    client_id: ClientId,
    provider_id: String,
    detection: Detection,
    expected_revision: String,
) -> Result<()> {
    r.provider_tests
        .set_detection(client_id, &provider_id, detection, &expected_revision)
}
pub fn provider_test_clear_mark(
    r: &Runtime,
    client_id: ClientId,
    provider_id: String,
    expected_revision: String,
) -> Result<()> {
    r.provider_tests
        .clear_mark(client_id, &provider_id, &expected_revision)
}
pub async fn provider_test_models(
    r: &Runtime,
    client_id: ClientId,
    provider_id: String,
    expected_revision: String,
    endpoint: Option<String>,
    refresh: Option<bool>,
) -> Result<Models> {
    r.provider_tests
        .models(
            client_id,
            &provider_id,
            &expected_revision,
            endpoint.as_deref(),
            refresh.unwrap_or(false),
        )
        .await
}
pub async fn provider_test_update_bank(r: &Runtime, engine: Engine) -> Result<Value> {
    match engine {
        Engine::ModelTrace => r.provider_tests.update_bank().await,
        Engine::LmDetector => r.provider_tests.update_lm_bank().await,
    }
}
