use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Days, NaiveTime, TimeDelta, Utc};
use gateway_core::engine::credential::ProviderAccountId;
use provider_openai::credential::{CodexUsageStatisticsError, CodexUsageStatisticsServiceTier};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{create_account, quota_service_with_base_url};
use crate::support::MemoryAccountStore;

const PERSONAL_MODEL_REPORT_PATH: &str = "/wham/usage/daily-token-usage-breakdown";
const PERSONAL_TOTALS_REPORT_PATH: &str = "/wham/analytics/daily-workspace-usage-counts";

fn cycle_anchor() -> (DateTime<Utc>, DateTime<Utc>) {
    let start_date = Utc::now()
        .date_naive()
        .checked_sub_days(Days::new(1))
        .expect("cycle start date");
    let start_at = DateTime::<Utc>::from_naive_utc_and_offset(
        start_date.and_time(NaiveTime::from_hms_opt(15, 50, 0).expect("cycle time")),
        Utc,
    );
    (
        start_at,
        start_at
            .checked_add_signed(TimeDelta::days(7))
            .expect("cycle end"),
    )
}

async fn mount_quota(server: &MockServer, reset_at: DateTime<Utc>) {
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "primary_window": {
                    "used_percent": 20,
                    "reset_at": reset_at.timestamp(),
                    "limit_window_seconds": 604800
                }
            }
        })))
        .mount(server)
        .await;
}

async fn service(
    account_id: &str,
    server: &MockServer,
    reset_at: DateTime<Utc>,
) -> (
    Arc<MemoryAccountStore>,
    provider_openai::credential::CodexCredentialQuotaService,
) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, account_id).await;
    mount_quota(server, reset_at).await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );
    (store, service)
}

#[tokio::test]
async fn personal_report_allocates_tokens_on_the_server_and_preserves_official_totals() {
    let server = MockServer::start().await;
    let (start_at, end_at) = cycle_anchor();
    let report_date = Utc::now().date_naive().to_string();
    Mock::given(method("GET"))
        .and(path(PERSONAL_MODEL_REPORT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{
            "date": report_date,
            "models": [
                {"model": "gpt-5.6-sol", "speed": "fast", "credits": 75},
                {"model": "gpt-5.6-terra", "speed": "standard", "credits": 25}
            ]
        }]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PERSONAL_TOTALS_REPORT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{
            "date": report_date,
            "uncached_text_input_tokens": 101,
            "cached_text_input_tokens": 203,
            "text_output_tokens": 17,
            "text_total_tokens": 321
        }]})))
        .mount(&server)
        .await;
    let (store, service) = service("acct_personal_statistics", &server, end_at).await;
    let account = store
        .account("acct_personal_statistics")
        .expect("personal account");

    let statistics = service
        .usage_statistics(account.id(), 0, 0)
        .await
        .expect("personal statistics");

    assert_eq!(statistics.cycle.start_at, start_at);
    assert_eq!(statistics.summary.tokens.uncached_input, 101);
    assert_eq!(statistics.summary.tokens.cached_input, 203);
    assert_eq!(statistics.summary.tokens.output, 17);
    assert_eq!(statistics.summary.tokens.total, 321);
    assert_eq!(
        statistics
            .models
            .iter()
            .map(|model| model.tokens.uncached_input)
            .sum::<u64>(),
        101
    );
    assert_eq!(
        statistics
            .models
            .iter()
            .map(|model| model.tokens.total)
            .sum::<u64>(),
        321
    );
    assert!(
        statistics
            .models
            .iter()
            .all(|model| model.has_estimated_allocation)
    );
    assert!(statistics.models.iter().any(|model| {
        model.model == "gpt-5.6-sol" && model.service_tier == CodexUsageStatisticsServiceTier::Fast
    }));
    assert!(statistics.models.iter().any(|model| {
        model.model == "gpt-5.6-terra"
            && model.service_tier == CodexUsageStatisticsServiceTier::Standard
    }));
}

#[tokio::test]
async fn cycle_offset_fetches_only_the_selected_cycle_with_boundary_padding() {
    let server = MockServer::start().await;
    let (current_start_at, current_end_at) = cycle_anchor();
    let selected_date = current_start_at
        .date_naive()
        .checked_sub_days(Days::new(14))
        .expect("selected date");
    Mock::given(method("GET"))
        .and(path(PERSONAL_MODEL_REPORT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{
            "date": selected_date.to_string(),
            "models": [{
                "model": "gpt-5.6-terra",
                "speed": "standard",
                "credits": 10
            }]
        }]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PERSONAL_TOTALS_REPORT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{
            "date": selected_date.to_string(),
            "uncached_text_input_tokens": 10,
            "text_total_tokens": 10
        }]})))
        .mount(&server)
        .await;
    let (store, service) = service("acct_offset_statistics", &server, current_end_at).await;
    let account = store
        .account("acct_offset_statistics")
        .expect("offset account");

    let statistics = service
        .usage_statistics(account.id(), -2, 0)
        .await
        .expect("historical statistics");
    assert_eq!(statistics.cycle.offset, -2);
    assert!(!statistics.cycle.is_current);
    assert!(statistics.cycle.can_go_next);
    assert!(statistics.cycle.used_percent.is_none());
    assert_eq!(statistics.daily[0].date, selected_date);

    let requests = server.received_requests().await.expect("recorded requests");
    let report = requests
        .iter()
        .find(|request| request.url.path() == PERSONAL_TOTALS_REPORT_PATH)
        .expect("personal totals report request");
    let query = report
        .url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<BTreeMap<_, _>>();
    let expected_start = current_start_at
        .date_naive()
        .checked_sub_days(Days::new(15))
        .expect("query start");
    let expected_end = current_start_at
        .date_naive()
        .checked_sub_days(Days::new(6))
        .expect("query end");
    assert_eq!(query.get("start_date"), Some(&expected_start.to_string()));
    assert_eq!(query.get("end_date"), Some(&expected_end.to_string()));
    assert_eq!(query.get("group_by").map(String::as_str), Some("day"));

    let invalid_account = ProviderAccountId::new("acct_invalid_offset").expect("account ID");
    assert!(matches!(
        service.usage_statistics(&invalid_account, -9, 0).await,
        Err(CodexUsageStatisticsError::InvalidRequest)
    ));
}
