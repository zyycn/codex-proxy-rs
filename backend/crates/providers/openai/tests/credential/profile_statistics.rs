use std::sync::Arc;

use chrono::{TimeZone as _, Utc};
use gateway_core::engine::credential::ProviderAccountId;
use provider_openai::credential::{CodexCredentialProfileService, ImportCodexOAuthCredential};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{MemoryAccountStore, profile, secret};

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "profile-statistics-contract".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 8, 28, 0, 0, 0)
            .single()
            .expect("fixture time"),
    })
}

async fn service(
    account_id: &str,
    server: &MockServer,
) -> (Arc<MemoryAccountStore>, CodexCredentialProfileService) {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret(&format!("token-{account_id}")),
            verified_account: profile(&format!("chatgpt-{account_id}")),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let service = CodexCredentialProfileService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );
    (store, service)
}

#[tokio::test]
async fn profile_statistics_uses_one_profile_request_and_preserves_official_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wham/profiles/me"))
        .and(header(
            "chatgpt-account-id",
            "chatgpt-acct_profile_statistics",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "profile": {
                "display_name": "  Ada  ",
                "username": " ada ",
                "profile_picture_url": " https://example.test/avatar.png "
            },
            "stats": {
                "lifetime_tokens": 409500000,
                "peak_daily_tokens": 267000000,
                "longest_running_turn_sec": 51900,
                "current_streak_days": 16,
                "longest_streak_days": 32,
                "daily_usage_buckets": [
                    {"start_date": "2026-08-18", "tokens": 20},
                    {"start_date": "2026-08-17", "tokens": 10}
                ],
                "fast_mode_usage_percentage": 0,
                "most_used_reasoning_effort": "high",
                "most_used_reasoning_effort_percentage": 48,
                "unique_skills_used": 8,
                "total_skills_used": 61,
                "total_threads": 2391,
                "top_invocations": [{
                    "type": "plugin",
                    "plugin_id": "plugin_1",
                    "plugin_name": "Example",
                    "usage_count": 29
                }]
            },
            "metadata": {"stats_error": null}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (store, service) = service("acct_profile_statistics", &server).await;
    let account = store
        .account("acct_profile_statistics")
        .expect("profile account");

    let statistics = service
        .profile_statistics(account.id())
        .await
        .expect("profile statistics");

    assert_eq!(statistics.display_name.as_deref(), Some("Ada"));
    assert_eq!(statistics.username.as_deref(), Some("ada"));
    assert_eq!(statistics.summary.total_text_tokens, Some(409_500_000));
    assert_eq!(
        statistics.summary.longest_task_duration_ms,
        Some(51_900_000)
    );
    assert_eq!(
        statistics.daily_usage.as_ref().expect("daily")[0].tokens,
        10
    );
    assert_eq!(
        statistics
            .activity_insights
            .invocations
            .as_ref()
            .expect("invocations")[0]
            .usage_count,
        Some(29)
    );
    assert!(!statistics.has_stats_error);
}

#[tokio::test]
async fn profile_statistics_keeps_profile_when_stats_are_unavailable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wham/profiles/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "profile": {"display_name": "Ada"},
            "stats": {},
            "metadata": {"stats_error": " aggregation unavailable "}
        })))
        .mount(&server)
        .await;
    let (_store, service) = service("acct_profile_partial", &server).await;
    let account_id = ProviderAccountId::new("acct_profile_partial").expect("account ID");

    let statistics = service
        .profile_statistics(&account_id)
        .await
        .expect("partial profile");

    assert_eq!(statistics.display_name.as_deref(), Some("Ada"));
    assert!(statistics.has_stats_error);
    assert_eq!(statistics.summary.total_text_tokens, None);
    assert_eq!(statistics.daily_usage, None);
}
