use gateway_admin::{
    model::{
        AdminErrorKind,
        plugins::distribution::{
            DownloadPurpose, GithubReleaseQuery, RemotePluginLocation, SourceAuthentication,
        },
    },
    ports::plugins::PluginDistribution,
};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

use super::{credential_with, digest, transport};

fn query(tag: Option<&str>) -> GithubReleaseQuery {
    GithubReleaseQuery {
        repository: "example/plugins".into(),
        tag: tag.map(str::to_owned),
        allow_prerelease: false,
    }
}

fn release() -> serde_json::Value {
    json!({"tag_name":"v1.0.0","name":"Plugin","draft":false,"prerelease":false,"assets":[{"id":1,"name":"example_1.0.0_linux_x86_64.tar.gz","size":7,"digest":format!("sha256:{}",digest(b"package"))}]})
}

#[tokio::test]
async fn release_without_published_checksum_can_be_previewed_with_a_computed_digest() {
    let server = MockServer::start().await;
    let mut metadata = release();
    metadata["assets"][0]["digest"] = serde_json::Value::Null;
    Mock::given(path("/repos/example/plugins/releases/tags/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
        .mount(&server)
        .await;
    Mock::given(path("/repos/example/plugins/releases/assets/1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .mount(&server)
        .await;
    let downloaded = transport(&server)
        .download(
            RemotePluginLocation::Github {
                repository: "example/plugins".into(),
                tag: "v1.0.0".into(),
                asset: "example_1.0.0_linux_x86_64.tar.gz".into(),
                allow_prerelease: false,
                sha256: None,
            },
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(downloaded.sha256, digest(b"package"));
}

#[tokio::test]
async fn github_credentials_authenticate_release_metadata_requests() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/example/plugins/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release()))
        .expect(1)
        .mount(&server)
        .await;
    let mut credential = credential_with(
        &server,
        "/repos/example/plugins",
        SourceAuthentication::Github {
            token: "test-github-token".into(),
        },
    );
    credential.info.purposes = vec![DownloadPurpose::Metadata];

    transport(&server)
        .query_release(query(None), vec![credential], None)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer test-github-token"
    );
}

#[tokio::test]
async fn concurrent_queries_share_a_single_fetch_and_return_cache_times() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/example/plugins/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(release())
                .set_delay(std::time::Duration::from_millis(30)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let distribution = transport(&server);
    let results = futures::future::join_all(
        (0..20).map(|_| distribution.query_release(query(None), vec![], None)),
    )
    .await;
    let first = results[0].as_ref().unwrap();
    for result in &results {
        let result = result.as_ref().unwrap();
        assert_eq!(result.tag, "v1.0.0");
        assert_eq!(result.queried_at, first.queried_at);
        assert_eq!((result.expires_at - result.queried_at).num_seconds(), 3600);
    }
}

#[tokio::test]
async fn explicit_queries_refresh_a_new_release_before_the_cache_expires() {
    let server = MockServer::start().await;
    let calls = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(path("/repos/example/plugins/releases/latest"))
        .respond_with(move |_: &wiremock::Request| {
            let mut metadata = release();
            if calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0 {
                metadata["tag_name"] = json!("model-trace-v0.1.3");
            }
            ResponseTemplate::new(200).set_body_json(metadata)
        })
        .expect(2)
        .mount(&server)
        .await;
    let distribution = transport(&server);
    let previous = distribution
        .query_release(query(None), vec![], None)
        .await
        .unwrap();
    let latest = distribution
        .query_release(query(None), vec![], None)
        .await
        .unwrap();
    assert_eq!(previous.tag, "v1.0.0");
    assert_eq!(latest.tag, "model-trace-v0.1.3");
    assert!(latest.queried_at < previous.expires_at);
}

#[tokio::test]
async fn artifact_downloads_reuse_the_checked_fixed_release() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/example/plugins/releases/tags/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/repos/example/plugins/releases/assets/1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .mount(&server)
        .await;
    let distribution = transport(&server);
    distribution
        .query_release(query(Some("v1.0.0")), vec![], None)
        .await
        .unwrap();
    let downloaded = distribution
        .download(
            RemotePluginLocation::Github {
                repository: "example/plugins".into(),
                tag: "v1.0.0".into(),
                asset: "example_1.0.0_linux_x86_64.tar.gz".into(),
                allow_prerelease: false,
                sha256: None,
            },
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(downloaded.sha256, digest(b"package"));
}

#[tokio::test]
async fn rate_limit_applies_to_other_repositories_using_the_same_identity_and_egress() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/example/plugins/releases/latest"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
        .expect(1)
        .mount(&server)
        .await;
    let distribution = transport(&server);
    let failure = distribution
        .query_release(query(None), vec![], None)
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), AdminErrorKind::RateLimited);
    let mut another = query(None);
    another.repository = "another/plugin".into();
    assert_eq!(
        distribution
            .query_release(another, vec![], None)
            .await
            .unwrap_err()
            .kind(),
        AdminErrorKind::RateLimited
    );
    assert_eq!(
        distribution
            .query_release(query(None), vec![], None)
            .await
            .unwrap_err(),
        failure
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn pre_releases_require_an_explicit_choice_and_mismatched_tags_are_rejected() {
    let server = MockServer::start().await;
    let mut preview = release();
    preview["prerelease"] = true.into();
    Mock::given(path("/repos/example/plugins/releases/tags/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(preview))
        .expect(2)
        .mount(&server)
        .await;
    let distribution = transport(&server);
    assert!(
        distribution
            .query_release(query(Some("v1.0.0")), vec![], None)
            .await
            .is_err()
    );
    let mut explicit = query(Some("v1.0.0"));
    explicit.allow_prerelease = true;
    assert!(
        distribution
            .query_release(explicit, vec![], None)
            .await
            .unwrap()
            .prerelease
    );
    Mock::given(path("/repos/example/plugins/releases/tags/v2.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release()))
        .mount(&server)
        .await;
    assert!(
        distribution
            .query_release(query(Some("v2.0.0")), vec![], None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn releases_without_asset_digest_use_exact_checksum_file_entry() {
    let server = MockServer::start().await;
    let mut metadata = release();
    metadata["assets"][0]["digest"] = serde_json::Value::Null;
    metadata["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":2,"name":"checksums.txt","size":200}));
    Mock::given(path("/repos/example/plugins/releases/tags/v1.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
        .mount(&server)
        .await;
    Mock::given(path("/repos/example/plugins/releases/assets/2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "{}  example_1.0.0_linux_x86_64.tar.gz\n",
            digest(b"package")
        )))
        .mount(&server)
        .await;
    Mock::given(path("/repos/example/plugins/releases/assets/1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .mount(&server)
        .await;
    let downloaded = transport(&server)
        .download(
            RemotePluginLocation::Github {
                repository: "example/plugins".into(),
                tag: "v1.0.0".into(),
                asset: "example_1.0.0_linux_x86_64.tar.gz".into(),
                allow_prerelease: false,
                sha256: None,
            },
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(&*downloaded.archive, b"package");
    assert_eq!(downloaded.sha256, digest(b"package"));
}

#[tokio::test]
async fn oversized_metadata_is_rejected_before_json_parsing() {
    let server = MockServer::start().await;
    Mock::given(path("/repos/example/plugins/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b' '; 1024 * 1024 + 1]))
        .mount(&server)
        .await;
    assert!(
        transport(&server)
            .query_release(query(None), vec![], None)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "实际访问 GitHub 公共仓库；通过 --ignored 显式运行"]
async fn live_github_resolves_latest_then_pins_the_same_release() {
    let distribution = gateway_host::plugin_distribution::HttpPluginDistribution::new(
        std::sync::Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
    )
    .unwrap();
    let mut query = GithubReleaseQuery {
        repository: "cli/cli".into(),
        tag: None,
        allow_prerelease: false,
    };
    let latest = distribution
        .query_release(query.clone(), vec![], None)
        .await
        .unwrap();
    assert!(!latest.prerelease);
    assert!(!latest.assets.is_empty());
    query.tag = Some(latest.tag.clone());
    let pinned = distribution
        .query_release(query, vec![], None)
        .await
        .unwrap();
    assert_eq!(pinned.tag, latest.tag);
    let checksums = pinned
        .assets
        .iter()
        .find(|asset| asset.name.ends_with("checksums.txt"))
        .unwrap();
    let expected = checksums
        .sha256
        .clone()
        .expect("GitHub Release checksum asset must have a published digest");
    let downloaded = distribution
        .download(
            RemotePluginLocation::Github {
                repository: pinned.repository,
                tag: pinned.tag,
                asset: checksums.name.clone(),
                allow_prerelease: false,
                sha256: Some(expected.clone()),
            },
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(downloaded.sha256, expected);
    assert!(!downloaded.archive.is_empty());
}
