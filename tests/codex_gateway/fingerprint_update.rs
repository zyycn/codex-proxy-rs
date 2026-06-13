use codex_proxy_rs::{
    codex::gateway::fingerprint::{
        repository::FingerprintRepository,
        updater::{FingerprintUpdater, CODEX_DESKTOP_UPDATE_SOURCE},
    },
    codex::gateway::transport::client::build_reqwest_client,
    platform::storage::db::connect_sqlite,
};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn fingerprint_updater_fetches_manifest_and_persists_history() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/desktop/update.json"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"version":"26.700.111","build_number":"5002"}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fingerprints.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    let repo = FingerprintRepository::new(pool);
    let updater = FingerprintUpdater::new(
        build_reqwest_client(false).unwrap(),
        repo.clone(),
        format!("{}/desktop/update.json", server.uri()),
    );

    updater.poll_once().await.unwrap();

    let latest = repo.latest().await.unwrap().unwrap();
    assert_eq!(latest.app_version, "26.700.111");
    assert_eq!(latest.build_number, "5002");
    assert_eq!(latest.source, CODEX_DESKTOP_UPDATE_SOURCE);
}
