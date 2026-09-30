use std::sync::Arc;

use gateway_admin::{
    model::{
        AdminErrorKind,
        plugins::distribution::{
            DownloadPurpose, RemotePluginLocation, SourceAuthentication, SourceCredential,
            SourceCredentialInfo,
        },
    },
    ports::plugins::PluginDistribution,
};
use gateway_host::{
    outbound::{HttpClient, NetworkPolicy},
    plugin_distribution::HttpPluginDistribution,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::{credential, credential_with, digest, transport};
use crate::support::network::{FixedDns, Mode, ROOT, Server};

#[tokio::test]
async fn https_http2_download_preserves_scoped_credentials_and_digest_verification() {
    let mut server = Server::start(Mode::Http2Origin, true, "127.0.0.1:0").await;
    let origin = format!("https://upstream.test:{}", server.address.port());
    let client = HttpClient::with_root_certificates(vec![ROOT.to_vec()])
        .unwrap()
        .with_resolver(Arc::new(FixedDns(server.address.ip())));
    let distribution = HttpPluginDistribution::with_transport(
        &origin,
        Arc::new(client),
        NetworkPolicy::new(&["127.0.0.0/8".into()]).unwrap(),
    )
    .unwrap();
    let result = distribution
        .download(
            RemotePluginLocation::Url {
                url: format!("{origin}/private/plugin.tar.gz"),
                sha256: Some(digest(b"secured")),
            },
            vec![SourceCredential {
                info: SourceCredentialInfo {
                    id: "test-credential".into(),
                    name: "Test".into(),
                    origin,
                    path_prefix: "/private".into(),
                    purposes: vec![DownloadPurpose::Artifact],
                },
                authentication: SourceAuthentication::Bearer {
                    token: "test-token".into(),
                },
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(&*result.archive, b"secured");
    assert!(
        server
            .next()
            .await
            .head
            .contains("authorization: Bearer test-token\r\n")
    );
}

#[tokio::test]
async fn source_failures_log_status_without_credentials_url_or_response_body() {
    use std::io::{Read as _, Seek as _, SeekFrom};

    use tracing::instrument::WithSubscriber as _;

    let server = MockServer::start().await;
    Mock::given(path("/private/plugin"))
        .respond_with(ResponseTemplate::new(400).set_body_string("private upstream response"))
        .expect(1)
        .mount(&server)
        .await;
    let mut output = tempfile::tempfile().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .without_time()
        .json()
        .with_writer(output.try_clone().unwrap())
        .finish();
    let error = transport(&server)
        .download(
            RemotePluginLocation::Url {
                url: format!("{}/private/plugin", server.uri()),
                sha256: None,
            },
            vec![credential(&server, "/private")],
            None,
        )
        .with_subscriber(subscriber)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminErrorKind::BadGateway);
    assert_eq!(
        error.to_string(),
        "插件来源请求失败，请检查来源与下载授权后重试"
    );
    output.seek(SeekFrom::Start(0)).unwrap();
    let mut log = String::new();
    output.read_to_string(&mut log).unwrap();
    let event: serde_json::Value = serde_json::from_str(&log).unwrap();
    assert_eq!(event["fields"]["status"], 400);
    for sensitive in ["test-token", "/private/plugin", "private upstream response"] {
        assert!(!log.contains(sensitive));
    }
}

#[tokio::test]
async fn url_preview_computes_digest_and_confirmation_rejects_replaced_content() {
    let server = MockServer::start().await;
    Mock::given(path("/plugin.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .expect(1)
        .mount(&server)
        .await;
    let distribution = transport(&server);
    let url = format!("{}/plugin.tar.gz", server.uri());
    let preview = distribution
        .download(
            RemotePluginLocation::Url {
                url: url.clone(),
                sha256: None,
            },
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(preview.sha256, digest(b"package"));

    server.reset().await;
    Mock::given(path("/plugin.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"replaced"))
        .expect(1)
        .mount(&server)
        .await;
    let result = distribution
        .download(
            RemotePluginLocation::Url {
                url,
                sha256: Some(preview.sha256),
            },
            vec![],
            None,
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn basic_credentials_attach_the_expected_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(path("/private/plugin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .expect(1)
        .mount(&server)
        .await;

    transport(&server)
        .download(
            RemotePluginLocation::Url {
                url: format!("{}/private/plugin", server.uri()),
                sha256: Some(digest(b"package")),
            },
            vec![credential_with(
                &server,
                "/private",
                SourceAuthentication::Basic {
                    username: "test-user".into(),
                    password: "test-password".into(),
                },
            )],
            None,
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Basic dGVzdC11c2VyOnRlc3QtcGFzc3dvcmQ="
    );
}

#[tokio::test]
async fn custom_credentials_attach_only_the_declared_safe_header() {
    let server = MockServer::start().await;
    Mock::given(path("/private/plugin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .expect(1)
        .mount(&server)
        .await;

    transport(&server)
        .download(
            RemotePluginLocation::Url {
                url: format!("{}/private/plugin", server.uri()),
                sha256: Some(digest(b"package")),
            },
            vec![credential_with(
                &server,
                "/private",
                SourceAuthentication::Header {
                    name: "X-Plugin-Token".into(),
                    value: "test-header-value".into(),
                },
            )],
            None,
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("x-plugin-token").unwrap(),
        "test-header-value"
    );
    assert!(!requests[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn url_download_verifies_external_digest_and_rejects_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/plugin.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .expect(2)
        .mount(&server)
        .await;
    let distribution = transport(&server);
    let result = distribution
        .download(
            RemotePluginLocation::Url {
                url: format!("{}/plugin.tar.gz", server.uri()),
                sha256: Some(digest(b"package")),
            },
            vec![],
            None,
        )
        .await
        .unwrap();
    assert_eq!(&*result.archive, b"package");
    assert!(
        distribution
            .download(
                RemotePluginLocation::Url {
                    url: format!("{}/plugin.tar.gz", server.uri()),
                    sha256: Some("0".repeat(64))
                },
                vec![],
                None,
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn redirects_recheck_both_origin_and_path_before_attaching_credentials() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(path("/private/plugin"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/public/plugin", first.uri())),
        )
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(path("/public/plugin"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/private/plugin", second.uri())),
        )
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(path("/private/plugin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .expect(1)
        .mount(&second)
        .await;
    transport(&first)
        .download(
            RemotePluginLocation::Url {
                url: format!("{}/private/plugin", first.uri()),
                sha256: Some(digest(b"package")),
            },
            vec![credential(&first, "/private")],
            None,
        )
        .await
        .unwrap();
    let requests = first.received_requests().await.unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer test-token"
    );
    assert!(!requests[1].headers.contains_key("authorization"));
    assert!(
        !second.received_requests().await.unwrap()[0]
            .headers
            .contains_key("authorization")
    );
}

#[tokio::test]
async fn credentials_are_applied_only_to_granted_purpose_and_unambiguous_scope() {
    use gateway_admin::model::plugins::distribution::DownloadPurpose;
    let server = MockServer::start().await;
    Mock::given(path("/package"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"package"))
        .expect(1)
        .mount(&server)
        .await;
    let mut metadata_only = credential(&server, "/");
    metadata_only.info.purposes = vec![DownloadPurpose::Metadata];
    let location = RemotePluginLocation::Url {
        url: format!("{}/package", server.uri()),
        sha256: Some(digest(b"package")),
    };
    let distribution = transport(&server);
    distribution
        .download(location.clone(), vec![metadata_only], None)
        .await
        .unwrap();
    assert!(
        !server.received_requests().await.unwrap()[0]
            .headers
            .contains_key("authorization")
    );
    assert!(
        distribution
            .download(
                location,
                vec![credential(&server, "/"), credential(&server, "/package")],
                None,
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn source_urls_cannot_persist_inline_credentials_or_non_https_remote_targets() {
    let server = MockServer::start().await;
    let distribution = transport(&server);
    for url in [
        "https://user:password@example.com/plugin",
        "https://example.com/plugin?token=secret",
        "http://example.com/plugin",
        "file:///etc/passwd",
    ] {
        assert!(
            distribution
                .download(
                    RemotePluginLocation::Url {
                        url: url.into(),
                        sha256: Some("0".repeat(64))
                    },
                    vec![],
                    None,
                )
                .await
                .is_err()
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn redirect_cycles_are_bounded() {
    let server = MockServer::start().await;
    Mock::given(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
        .expect(6)
        .mount(&server)
        .await;
    assert!(
        transport(&server)
            .download(
                RemotePluginLocation::Url {
                    url: format!("{}/loop", server.uri()),
                    sha256: Some("0".repeat(64))
                },
                vec![],
                None,
            )
            .await
            .is_err()
    );
}
