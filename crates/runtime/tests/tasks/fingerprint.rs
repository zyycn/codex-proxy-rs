use super::*;

#[tokio::test]
async fn fingerprint_update_task_should_start_background_checker() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);
    repo.ensure_current_seed(
        &codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .await
    .expect("seed current fingerprint");

    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        Some(repo),
        "https://example.invalid/appcast.xml".to_string(),
        dir.path().join("extracted-fingerprint.json"),
        "26.800.1".to_string(),
        "6001".to_string(),
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn fingerprint_update_task_should_apply_initial_appcast_update_to_repository() {
    let server = MockServer::start().await;
    mount_appcast(
        &server,
        r#"
        <rss>
          <channel>
            <item>
              <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
            </item>
          </channel>
        </rss>
        "#,
    )
    .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints-initial-update.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);
    repo.ensure_current_seed(
        &codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .await
    .expect("seed current fingerprint");
    let extracted_path = dir.path().join("extracted-fingerprint.json");
    std::fs::write(
        &extracted_path,
        r#"{"app_version":"26.900.1","build_number":"7001","chromium_version":"147"}"#,
    )
    .expect("extracted fingerprint should be written");

    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        Some(repo.clone()),
        format!("{}/appcast.xml", server.uri()),
        extracted_path,
        "26.800.1".to_string(),
        "6001".to_string(),
    );

    let stored = wait_for_current_fingerprint_version(&repo).await;
    handle.shutdown().await;

    assert_eq!(
        (
            stored.app_version.as_str(),
            stored.build_number.as_str(),
            stored.chromium_version.as_str()
        ),
        ("26.900.1", "7001", "147")
    );
}

#[tokio::test]
async fn fingerprint_update_task_should_not_persist_when_appcast_matches_current_fingerprint() {
    let server = MockServer::start().await;
    mount_appcast(
        &server,
        r#"
        <rss>
          <channel>
            <item>
              <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
            </item>
          </channel>
        </rss>
        "#,
    )
    .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints-no-update.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = FingerprintRepository::new(pool);
    repo.ensure_current_seed(
        &codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .await
    .expect("seed current fingerprint");
    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        Some(repo.clone()),
        format!("{}/appcast.xml", server.uri()),
        dir.path().join("missing-extracted-fingerprint.json"),
        "26.900.1".to_string(),
        "7001".to_string(),
    );

    wait_for_appcast_requests(&server, 1).await;
    handle.shutdown().await;
    let latest_history = repo.latest().await.expect("latest history should load");

    assert!(latest_history.is_none());
}

#[tokio::test]
async fn fingerprint_update_task_should_check_appcast_without_repository() {
    let server = MockServer::start().await;
    mount_appcast(
        &server,
        r#"
        <rss>
          <channel>
            <item>
              <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
            </item>
          </channel>
        </rss>
        "#,
    )
    .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let handle = codex_proxy_runtime::tasks::fingerprint_update::start_fingerprint_update_task(
        None,
        format!("{}/appcast.xml", server.uri()),
        dir.path().join("missing-extracted-fingerprint.json"),
        "26.800.1".to_string(),
        "6001".to_string(),
    );

    wait_for_appcast_requests(&server, 1).await;
    handle.shutdown().await;
}
