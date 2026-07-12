use std::{num::NonZeroU32, time::Duration};

use codex_proxy_rs::upstream::openai::fingerprint::{
    APPCAST_POLL_INTERVAL, PgFingerprintStore, UpdateChecker,
};

use crate::support::storage::init_test_db;

#[test]
fn fingerprint_appcast_poll_interval_should_be_one_day() {
    assert_eq!(APPCAST_POLL_INTERVAL, Duration::from_hours(24));
}

#[tokio::test]
async fn fingerprint_store_should_update_current_record() {
    let (pool, _guard) = init_test_db("fingerprint-store").await;
    let repo = PgFingerprintStore::new(pool.clone());
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
async fn fingerprint_store_should_keep_latest_update_history_rows() {
    let (pool, _guard) = init_test_db("fingerprint-history-limit").await;
    let repo = PgFingerprintStore::new(pool.clone());
    repo.ensure_current_seed(&crate::support::fingerprint::test_fingerprint())
        .await
        .expect("seed current fingerprint");
    sqlx::query(
        "insert into fingerprint_update_history (
           id, current_fingerprint_id, app_version, build_number, source, created_at
         )
         select
           'history-' || lpad(sequence::text, 3, '0'),
           'current',
           '26.900.' || sequence::text,
           sequence::text,
           'auto_update',
           $1 + sequence * interval '1 second'
         from generate_series(0, 101) as sequence",
    )
    .bind(chrono::Utc::now())
    .execute(&pool)
    .await
    .expect("insert fingerprint history rows");

    let deleted = repo
        .trim_update_history_to_limit(NonZeroU32::new(100).unwrap())
        .await
        .expect("trim fingerprint history rows");
    let (remaining, oldest_remaining): (i64, i64) = sqlx::query_as(
        "select
           count(*),
           count(*) filter (where id in ('history-000', 'history-001'))
         from fingerprint_update_history",
    )
    .fetch_one(&pool)
    .await
    .expect("count retained fingerprint history rows");

    assert_eq!((deleted, remaining, oldest_remaining), (2, 100, 0));
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

    let dir = tempfile::tempdir().expect("temp dir");
    let (pool, _guard) = init_test_db("fingerprint-update-check").await;
    let repo = PgFingerprintStore::new(pool);
    repo.ensure_current_seed(&crate::support::fingerprint::test_fingerprint())
        .await
        .expect("seed current fingerprint");

    let checker = UpdateChecker::with_client(
        repo,
        reqwest::Client::new(),
        format!("{}/appcast.xml", server.uri()),
        dir.path().join("extracted-fingerprint.json"),
        "26.800.1",
        "6001",
    );

    let state = checker.check_for_update().await.expect("update state");

    assert!(state.update_available);
    assert_eq!(state.latest_version.as_deref(), Some("26.900.1"));
    assert_eq!(state.latest_build.as_deref(), Some("7001"));
}

#[tokio::test]
async fn update_checker_should_apply_available_update_to_store() {
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
    let (pool, _guard) = init_test_db("fingerprint-update-apply").await;
    let repo = PgFingerprintStore::new(pool);
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
        repo.clone(),
        reqwest::Client::new(),
        format!("{}/appcast.xml", server.uri()),
        extracted_path,
        "26.800.1",
        "6001",
    );

    let updated = checker
        .check_and_apply_update()
        .await
        .expect("apply update")
        .expect("available update should return current fingerprint");
    let stored = repo
        .load_current()
        .await
        .expect("load current")
        .expect("stored fingerprint");

    assert_eq!(updated.app_version, "26.900.1");
    assert_eq!(updated.build_number, "7001");
    assert_eq!(updated.chromium_version, "147");
    assert_eq!(stored.app_version, "26.900.1");
    assert_eq!(stored.build_number, "7001");
    assert_eq!(stored.chromium_version, "147");
}
