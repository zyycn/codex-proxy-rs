use codex_proxy_rs::infra::database::connect_sqlite;
use codex_proxy_rs::upstream::fingerprint::{FingerprintRepository, UpdateChecker};

#[tokio::test]
async fn fingerprint_repository_should_update_current_record() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool.clone());
    let default_fingerprint = crate::support::fingerprint::test_fingerprint();

    repo.ensure_current_seed(&default_fingerprint)
        .await
        .expect("seed current fingerprint");
    repo.update_current_version("26.800.1", "6001", Some("147"))
        .await
        .expect("first update");
    repo.update_current_version("26.800.2", "6002", None)
        .await
        .expect("second update");

    let stored = repo
        .load_current()
        .await
        .expect("load current")
        .expect("stored fingerprint");
    let count: (i64,) = sqlx::query_as("select count(*) from fingerprints where id = 'current'")
        .fetch_one(&pool)
        .await
        .expect("count row");

    assert_eq!(count.0, 1);
    assert_eq!(stored.app_version, "26.800.2");
    assert_eq!(stored.build_number, "6002");
    assert_eq!(stored.chromium_version, "147");
}

#[tokio::test]
async fn update_checker_should_report_available_update_from_appcast() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/appcast.xml"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(
            r#"
            <rss>
              <channel>
                <item>
                  <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
                </item>
              </channel>
            </rss>
            "#,
            "application/xml",
        ))
        .mount(&server)
        .await;

    let checker = UpdateChecker::with_client(
        None,
        reqwest::Client::new(),
        format!("{}/appcast.xml", server.uri()),
        tempfile::tempdir()
            .expect("temp dir")
            .path()
            .join("extracted-fingerprint.json"),
        "26.800.1",
        "6001",
    );

    let state = checker.check_for_update().await.expect("update state");

    assert!(state.update_available);
    assert_eq!(state.latest_version.as_deref(), Some("26.900.1"));
    assert_eq!(state.latest_build.as_deref(), Some("7001"));
}

#[tokio::test]
async fn update_checker_should_apply_available_update_to_repository() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/appcast.xml"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(
            r#"
            <rss>
              <channel>
                <item>
                  <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
                </item>
              </channel>
            </rss>
            "#,
            "application/xml",
        ))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);
    repo.ensure_current_seed(&crate::support::fingerprint::test_fingerprint())
        .await
        .expect("seed current fingerprint");
    let extracted_path = dir.path().join("extracted-fingerprint.json");
    std::fs::write(
        &extracted_path,
        r#"{"app_version":"26.900.1","build_number":"7001","chromium_version":"147"}"#,
    )
    .expect("write extracted fingerprint");

    let checker = UpdateChecker::with_client(
        Some(repo.clone()),
        reqwest::Client::new(),
        format!("{}/appcast.xml", server.uri()),
        extracted_path,
        "26.800.1",
        "6001",
    );

    let applied = checker
        .check_and_apply_update()
        .await
        .expect("apply update");
    let stored = repo
        .load_current()
        .await
        .expect("load current")
        .expect("stored fingerprint");

    assert!(applied);
    assert_eq!(stored.app_version, "26.900.1");
    assert_eq!(stored.build_number, "7001");
    assert_eq!(stored.chromium_version, "147");
}
