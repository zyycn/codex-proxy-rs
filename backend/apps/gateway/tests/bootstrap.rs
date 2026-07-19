use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, SystemTime},
};

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::CONTENT_TYPE},
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use codex_proxy_rs::{
    BootstrapConfig, ConfigError, InMemoryCodexOAuthPendingStore, ProcessSystemAdminService,
    SystemUpdateConfig, TopologyOverrides, external_observability_range, health_timeline_view_at,
    provider_failure_affects_circuit, restore_client_admission_startup,
    runtime_revision_needs_refresh,
};
use flate2::{Compression, write::GzEncoder};
use gateway_api::admin::{
    AdminServiceError, AdminSessionResolver, AdminSessionState,
    system::{self, SystemAdminErrorKind, SystemAdminService, SystemAdminState},
};
use gateway_core::error::ProviderErrorKind;
use gateway_store::postgres::{ObservationGranularity, RequestMetricPoint, RequestMetrics};
use provider_openai::credential::{
    CodexOAuthPendingStore, CodexPendingAuthorization, StoredCodexPendingAuthorization,
};
use secrecy::SecretString;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tar::{Builder, EntryType, Header};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const CONFIG_EXAMPLE: &str = include_str!("../../../../deploy/config.example.yaml");
const POSTGRES_PASSWORD: &str = "111111111111111111111111111111111111111111111111";
const REDIS_PASSWORD: &str = "222222222222222222222222222222222222222222222222";
const ADMIN_PASSWORD: &str = "test-admin-password";

#[test]
fn health_timeline_should_keep_exactly_china_day_quarter_hour_slots() {
    let now = utc_time(2026, 7, 19, 2, 22);
    let points = vec![
        health_metric_point(
            utc_time(2026, 7, 18, 15, 45),
            RequestMetrics {
                success_count: 100,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            utc_time(2026, 7, 19, 2, 15),
            RequestMetrics {
                success_count: 2,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            utc_time(2026, 7, 19, 2, 30),
            RequestMetrics {
                success_count: 100,
                ..RequestMetrics::default()
            },
        ),
    ];

    let timeline = health_timeline_view_at(&points, now);

    assert_eq!(timeline.description, "有效请求可用性");
    assert_eq!(timeline.points.len(), 96);
    assert_eq!(
        timeline.points.first().map(|point| point.time.as_str()),
        Some("00:00")
    );
    assert_eq!(
        timeline.points.last().map(|point| point.time.as_str()),
        Some("23:45")
    );
    assert_eq!(timeline.success_requests, 2);
    assert_eq!(timeline.points[41].status, "low_sample");
    assert_eq!(timeline.points[42].status, "future");
    assert_eq!(timeline.points[42].success_requests, 0);
}

#[test]
fn health_timeline_should_match_legacy_status_precedence_and_thresholds() {
    let now = utc_time(2026, 7, 19, 4, 0);
    let points = vec![
        health_metric_point(
            utc_time(2026, 7, 18, 16, 0),
            RequestMetrics {
                failure_count: 1,
                cancelled_count: 1,
                incomplete_count: 1,
                caller_error_count: 1,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            utc_time(2026, 7, 18, 16, 15),
            RequestMetrics {
                failure_count: 3,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            utc_time(2026, 7, 18, 16, 30),
            RequestMetrics {
                success_count: 1,
                failure_count: 1,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            utc_time(2026, 7, 18, 16, 45),
            RequestMetrics {
                success_count: 98,
                failure_count: 2,
                ..RequestMetrics::default()
            },
        ),
        health_metric_point(
            utc_time(2026, 7, 18, 17, 0),
            RequestMetrics {
                success_count: 99,
                failure_count: 1,
                ..RequestMetrics::default()
            },
        ),
    ];

    let timeline = health_timeline_view_at(&points, now);
    let statuses = timeline
        .points
        .iter()
        .take(5)
        .map(|point| point.status.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        statuses,
        vec!["no_data", "unavailable", "low_sample", "unstable", "stable"]
    );
    assert_eq!(timeline.points[0].reliability_display, "-");
    assert_eq!(timeline.points[4].reliability_display, "99.0%");
}

fn utc_time(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("valid UTC test time")
}

fn health_metric_point(bucket_start: DateTime<Utc>, metrics: RequestMetrics) -> RequestMetricPoint {
    RequestMetricPoint {
        bucket_start,
        granularity: ObservationGranularity::FifteenMinutes,
        metrics,
        cost_coverage: gateway_store::postgres::CostCoverage::default(),
        costs: Vec::new(),
    }
}

#[tokio::test]
async fn codex_pending_flow_wrong_owner_does_not_consume_it() {
    let store = InMemoryCodexOAuthPendingStore::new();
    let pending = CodexPendingAuthorization::from_stored(StoredCodexPendingAuthorization {
        flow_id: "flow_owner_bound".to_owned(),
        owner_ref: "admin:owner".to_owned(),
        started_request_ref: "request_owner_bound".to_owned(),
        provider_instance_id: "provider_openai".to_owned(),
        name: "Owner-bound OAuth".to_owned(),
        expires_at: Utc::now() + Duration::minutes(10),
        state: SecretString::from("state-owner-bound"),
        nonce: SecretString::from("nonce-owner-bound"),
        code_verifier: SecretString::from("verifier-owner-bound"),
        reauthorization_account_id: None,
        reauthorization_credential_revision: None,
    })
    .expect("valid pending Codex OAuth flow");
    store.create(&pending).await.expect("store pending flow");

    assert!(
        store
            .take("admin:other", "flow_owner_bound")
            .await
            .expect("wrong owner lookup")
            .is_none()
    );
    assert_eq!(
        store
            .take("admin:owner", "flow_owner_bound")
            .await
            .expect("owner lookup")
            .map(|flow| flow.owner_ref().to_owned()),
        Some("admin:owner".to_owned())
    );
    assert!(
        store
            .take("admin:owner", "flow_owner_bound")
            .await
            .expect("single-use lookup")
            .is_none()
    );
}

#[test]
fn external_observability_range_accepts_exactly_366_days() {
    let end = Utc::now();
    let range = external_observability_range(end - Duration::days(366), end)
        .expect("366-day external range should be accepted");
    assert_eq!(range.end, end);
}

#[test]
fn external_observability_range_rejects_over_366_days_and_reversed_range() {
    let end = Utc::now();
    assert!(external_observability_range(end - Duration::days(367), end).is_err());
    assert!(external_observability_range(end, end).is_err());
    assert!(external_observability_range(end + Duration::seconds(1), end).is_err());
}

#[test]
fn provider_circuit_should_only_count_instance_attributable_failures() {
    for error_kind in [
        ProviderErrorKind::Timeout,
        ProviderErrorKind::Transport,
        ProviderErrorKind::Protocol,
        ProviderErrorKind::Unavailable,
    ] {
        assert!(provider_failure_affects_circuit(error_kind));
    }
    for error_kind in [
        ProviderErrorKind::InvalidRequest,
        ProviderErrorKind::Unsupported,
        ProviderErrorKind::Unauthorized,
        ProviderErrorKind::PermissionDenied,
        ProviderErrorKind::RateLimited,
        ProviderErrorKind::QuotaExhausted,
        ProviderErrorKind::Cancelled,
        ProviderErrorKind::ProcessTerminated,
    ] {
        assert!(!provider_failure_affects_circuit(error_kind));
    }
}

#[test]
fn runtime_revision_reconciliation_should_refresh_missing_or_stale_snapshot() {
    assert!(!runtime_revision_needs_refresh(Some(7), 7));
    assert!(runtime_revision_needs_refresh(Some(6), 7));
    assert!(runtime_revision_needs_refresh(Some(8), 7));
    assert!(runtime_revision_needs_refresh(None, 7));
}

#[test]
fn config_loader_should_load_complete_terminal_example() {
    let directory = write_config(&complete_config());
    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml"))
        .expect("terminal configuration should load");

    assert_eq!(
        (
            loaded.app().server.host.as_str(),
            loaded.app().server.port,
            loaded.app().wire_profile.codex_version.as_str(),
            loaded.app().wire_profile.desktop_version.as_str(),
            loaded.app().wire_profile.desktop_build.as_str(),
            loaded.app().admin.default_username.as_str(),
        ),
        (
            "127.0.0.1",
            8080,
            "0.144.2",
            "26.707.72221",
            "5307",
            "admin@cpr.local",
        )
    );
}

#[test]
fn config_loader_should_resolve_paths_relative_to_config_file() {
    let directory = write_config(&complete_config());
    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml"))
        .expect("terminal configuration should load");

    assert_eq!(
        loaded.app().logging.file.directory.as_path(),
        directory.path().join("../.runtime/logs").as_path()
    );
}

#[test]
fn config_loader_should_inject_service_passwords_into_connection_urls() {
    let directory = write_config(&complete_config());
    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml"))
        .expect("terminal configuration should load");

    assert_eq!(
        (loaded.database_url(), loaded.redis_url()),
        (
            "postgres://codex_proxy:111111111111111111111111111111111111111111111111@127.0.0.1:5432/codex_proxy",
            "redis://:222222222222222222222222222222222222222222222222@127.0.0.1:6379/",
        )
    );
}

#[test]
fn config_loader_should_apply_only_explicit_topology_overrides() {
    let directory = write_config(&complete_config());
    let overrides = TopologyOverrides {
        server_host: Some("0.0.0.0".to_string()),
        server_port: Some(18080),
        database_url: Some("postgres://codex_proxy@postgres:5432/codex_proxy".to_string()),
        redis_url: Some("redis://redis:6379/".to_string()),
    };

    let loaded = BootstrapConfig::load_from_path_with_overrides(
        directory.path().join("config.yaml"),
        overrides,
    )
    .expect("explicit topology overrides should load");

    assert_eq!(loaded.app().server.host, "0.0.0.0");
    assert_eq!(loaded.app().server.port, 18080);
    assert_eq!(
        loaded.database_url(),
        "postgres://codex_proxy:111111111111111111111111111111111111111111111111@postgres:5432/codex_proxy"
    );
    assert_eq!(
        loaded.redis_url(),
        "redis://:222222222222222222222222222222222222222222222222@redis:6379/"
    );
}

#[test]
fn bootstrap_config_debug_should_redact_all_passwords() {
    let directory = write_config(&complete_config());
    let loaded = BootstrapConfig::load_from_path(directory.path().join("config.yaml"))
        .expect("terminal configuration should load");
    let debug = format!("{loaded:?}");

    assert!(!debug.contains(POSTGRES_PASSWORD));
    assert!(!debug.contains(REDIS_PASSWORD));
    assert!(!debug.contains(ADMIN_PASSWORD));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn config_loader_should_reject_unknown_fields() {
    let yaml = complete_config().replace("    port: 8080", "    port: 8080\n    unexpected: true");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidDocument { .. })
    ));
}

#[test]
fn config_loader_should_reject_removed_tls_section() {
    let yaml = complete_config().replace(
        "  wire_profile:",
        "  tls:\n    force_http11: false\n\n  wire_profile:",
    );
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidDocument { .. })
    ));
}

#[test]
fn config_loader_should_reject_missing_explicit_fields() {
    let yaml = complete_config().replacen("  logging:\n", "", 1);
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidDocument { .. })
    ));
}

#[test]
fn config_loader_should_reject_unsupported_schema_version() {
    let yaml = complete_config().replace("schema_version: 1", "schema_version: 0");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::UnsupportedSchemaVersion)
    ));
}

#[test]
fn config_loader_should_reject_embedded_database_password() {
    let yaml = complete_config().replace(
        "postgres://codex_proxy@127.0.0.1",
        "postgres://codex_proxy:duplicated@127.0.0.1",
    );
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::PasswordInUrl("database.url"))
    ));
}

#[test]
fn config_loader_should_reject_non_hex_postgres_password() {
    let yaml = complete_config().replace(POSTGRES_PASSWORD, "not-a-hex-password");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidServicePassword("database.password"))
    ));
}

#[test]
fn config_loader_should_reject_wrong_length_redis_password() {
    let yaml = complete_config().replace(REDIS_PASSWORD, "2222222222222222");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidServicePassword("redis.password"))
    ));
}

#[test]
fn config_loader_should_reject_weak_admin_password() {
    let yaml = complete_config().replace(ADMIN_PASSWORD, "password");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::WeakAdminPassword)
    ));
}

#[test]
fn config_loader_should_reject_admin_password_with_compose_interpolation() {
    let yaml = complete_config().replace(ADMIN_PASSWORD, "strong$password-value");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::WeakAdminPassword)
    ));
}

#[test]
fn config_loader_should_reject_removed_fingerprint_section() {
    let yaml = complete_config().replace("  wire_profile:", "  fingerprint:");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidDocument { .. })
    ));
}

#[test]
fn config_loader_should_reject_invalid_codex_core_version() {
    let yaml = complete_config().replace("codex_version: '0.144.2'", "codex_version: 'Desktop'");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidField("wire_profile.codex_version"))
    ));
}

#[test]
fn config_loader_should_reject_disabled_all_log_outputs() {
    let yaml = complete_config()
        .replace("    stdout: true", "    stdout: false")
        .replace("      enabled: true", "      enabled: false");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidField("logging"))
    ));
}

#[test]
fn config_loader_should_reject_zero_server_port() {
    let yaml = complete_config().replace("    port: 8080", "    port: 0");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidField("server.port"))
    ));
}

#[test]
fn config_loader_should_reject_invalid_desktop_profile_fields() {
    let yaml = complete_config().replace("desktop_build: '5307'", "desktop_build: 'build'");
    assert!(matches!(
        load_yaml(&yaml),
        Err(ConfigError::InvalidField("wire_profile.desktop_build"))
    ));
}

fn complete_config() -> String {
    CONFIG_EXAMPLE
        .replace(
            "password: &postgres_password ''",
            &format!("password: &postgres_password '{POSTGRES_PASSWORD}'"),
        )
        .replace(
            "password: &redis_password ''",
            &format!("password: &redis_password '{REDIS_PASSWORD}'"),
        )
        .replace(
            "default_password: ''",
            &format!("default_password: '{ADMIN_PASSWORD}'"),
        )
}

fn load_yaml(yaml: &str) -> Result<BootstrapConfig, ConfigError> {
    let directory = write_config(yaml);
    BootstrapConfig::load_from_path(directory.path().join("config.yaml"))
}

fn write_config(yaml: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary configuration directory");
    fs::write(directory.path().join("config.yaml"), yaml).expect("write test configuration");
    directory
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupFailure {
    Recover,
    Load,
    Restore,
}

#[derive(Default)]
struct StartupRecoveryState {
    calls: Mutex<Vec<&'static str>>,
    restored: Mutex<Vec<gateway_store::redis::ClientAdmissionRestore>>,
    window_started_at: Mutex<Option<chrono::DateTime<Utc>>>,
}

struct StartupModelRequests {
    state: Arc<StartupRecoveryState>,
    failure: Option<StartupFailure>,
}

#[async_trait]
impl gateway_store::postgres::ModelRequestRepository for StartupModelRequests {
    async fn insert_model_request(
        &self,
        _request: gateway_store::postgres::NewModelRequest,
    ) -> gateway_store::StoreResult<()> {
        unreachable!("startup recovery never inserts requests")
    }

    async fn begin_model_request_attempt(
        &self,
        _attempt: gateway_store::postgres::ModelRequestAttemptStart,
    ) -> gateway_store::StoreResult<u32> {
        unreachable!("startup recovery never starts attempts")
    }

    async fn mark_upstream_send_state(
        &self,
        _model_request_id: &str,
        _state: gateway_store::postgres::UpstreamSendState,
    ) -> gateway_store::StoreResult<bool> {
        unreachable!("startup recovery never changes send state")
    }

    async fn mark_downstream_committed(
        &self,
        _model_request_id: &str,
        _committed_at: chrono::DateTime<Utc>,
        _client_status_code: Option<u16>,
    ) -> gateway_store::StoreResult<bool> {
        unreachable!("startup recovery never commits downstream delivery")
    }

    async fn record_client_status_code(
        &self,
        _model_request_id: &str,
        _client_status_code: u16,
    ) -> gateway_store::StoreResult<bool> {
        unreachable!("startup recovery never records client status")
    }

    async fn finalize_model_request(
        &self,
        _finalization: gateway_store::postgres::ModelRequestFinalization,
    ) -> gateway_store::StoreResult<bool> {
        unreachable!("startup recovery never finalizes live requests")
    }

    async fn recover_expired_model_requests(
        &self,
        _now: chrono::DateTime<Utc>,
    ) -> gateway_store::StoreResult<gateway_store::postgres::ModelRequestRecoveryReport> {
        self.state.calls.lock().expect("calls lock").push("recover");
        if self.failure == Some(StartupFailure::Recover) {
            return Err(startup_store_error(gateway_store::StoreBackend::PostgreSql));
        }
        Ok(gateway_store::postgres::ModelRequestRecoveryReport { requests: 2 })
    }
}

struct StartupRecoveryRepository {
    state: Arc<StartupRecoveryState>,
    failure: Option<StartupFailure>,
    now: chrono::DateTime<Utc>,
}

#[async_trait]
impl gateway_store::postgres::ClientAdmissionRecoveryRepository for StartupRecoveryRepository {
    async fn load_client_admission_recovery(
        &self,
        window_started_at: chrono::DateTime<Utc>,
    ) -> gateway_store::StoreResult<Vec<gateway_store::postgres::ClientAdmissionRecovery>> {
        self.state.calls.lock().expect("calls lock").push("load");
        *self.state.window_started_at.lock().expect("window lock") = Some(window_started_at);
        if self.failure == Some(StartupFailure::Load) {
            return Err(startup_store_error(gateway_store::StoreBackend::PostgreSql));
        }
        Ok(vec![gateway_store::postgres::ClientAdmissionRecovery {
            client_api_key_ref: "key_client_1".to_owned(),
            recent_requests: vec![gateway_store::postgres::ClientAdmissionRecentRequest {
                model_request_id: "req_recent_1".to_owned(),
                started_at: self.now - Duration::seconds(30),
                input_token_estimate: 12,
            }],
            running_requests: vec![gateway_store::postgres::ClientAdmissionRunningRequest {
                model_request_id: "req_running_1".to_owned(),
                deadline_at: self.now + Duration::seconds(60),
            }],
        }])
    }
}

struct StartupAdmissions {
    state: Arc<StartupRecoveryState>,
    failure: Option<StartupFailure>,
}

#[async_trait]
impl gateway_store::redis::ClientAdmissionRepository for StartupAdmissions {
    async fn admit_client_request(
        &self,
        _request: &gateway_store::redis::ClientAdmissionRequest,
    ) -> gateway_store::StoreResult<gateway_store::redis::ClientAdmissionDecision> {
        unreachable!("startup recovery never admits a new request")
    }

    async fn release_client_request(
        &self,
        _client_api_key_ref: &str,
        _model_request_id: &str,
    ) -> gateway_store::StoreResult<bool> {
        unreachable!("startup recovery never releases a request")
    }

    async fn restore_client_admission(
        &self,
        recovery: &gateway_store::redis::ClientAdmissionRestore,
    ) -> gateway_store::StoreResult<gateway_store::redis::ClientAdmissionRestoreResult> {
        self.state.calls.lock().expect("calls lock").push("restore");
        self.state
            .restored
            .lock()
            .expect("restored lock")
            .push(recovery.clone());
        if self.failure == Some(StartupFailure::Restore) {
            return Err(startup_store_error(gateway_store::StoreBackend::Redis));
        }
        Ok(gateway_store::redis::ClientAdmissionRestoreResult {
            restored_recent_requests: 1,
            restored_running_requests: 1,
        })
    }

    async fn clear_client_admission(
        &self,
        _client_api_key_ref: &str,
    ) -> gateway_store::StoreResult<()> {
        unreachable!("startup recovery never clears client state")
    }
}

#[tokio::test]
async fn client_admission_startup_recovery_should_preserve_order_and_exact_facts() {
    let now = Utc::now();
    let state = Arc::new(StartupRecoveryState::default());
    let model_requests = StartupModelRequests {
        state: Arc::clone(&state),
        failure: None,
    };
    let recovery = StartupRecoveryRepository {
        state: Arc::clone(&state),
        failure: None,
        now,
    };
    let admissions = StartupAdmissions {
        state: Arc::clone(&state),
        failure: None,
    };

    let report = restore_client_admission_startup(&model_requests, &recovery, &admissions, now)
        .await
        .expect("startup recovery");

    assert_eq!(
        *state.calls.lock().expect("calls lock"),
        ["recover", "load", "restore"]
    );
    assert_eq!(
        *state.window_started_at.lock().expect("window lock"),
        Some(now - Duration::seconds(61))
    );
    let restored = state.restored.lock().expect("restored lock");
    assert_eq!(restored[0].recent_requests[0].input_token_estimate, 12);
    assert_eq!(
        restored[0].running_requests[0].expires_at,
        now + Duration::seconds(60)
    );
    assert_eq!(report.expired_model_requests, 2);
    assert_eq!(report.restored_clients, 1);
    assert_eq!(report.restored_recent_requests, 1);
    assert_eq!(report.restored_running_requests, 1);
}

#[tokio::test]
async fn client_admission_startup_recovery_should_fail_closed_at_each_boundary() {
    let now = Utc::now();
    for (failure, expected_calls) in [
        (StartupFailure::Recover, vec!["recover"]),
        (StartupFailure::Load, vec!["recover", "load"]),
        (StartupFailure::Restore, vec!["recover", "load", "restore"]),
    ] {
        let state = Arc::new(StartupRecoveryState::default());
        let result = restore_client_admission_startup(
            &StartupModelRequests {
                state: Arc::clone(&state),
                failure: Some(failure),
            },
            &StartupRecoveryRepository {
                state: Arc::clone(&state),
                failure: Some(failure),
                now,
            },
            &StartupAdmissions {
                state: Arc::clone(&state),
                failure: Some(failure),
            },
            now,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(*state.calls.lock().expect("calls lock"), expected_calls);
    }
}

fn startup_store_error(backend: gateway_store::StoreBackend) -> gateway_store::StoreError {
    gateway_store::StoreError::Unavailable {
        backend,
        message: "startup recovery unavailable".to_owned(),
    }
}

const SYSTEM_VERSION_OLD: &str = "0.1.0";
const SYSTEM_VERSION_NEW: &str = "0.2.0";

#[tokio::test]
async fn version_should_return_backend_build_metadata() {
    let repository = "zyycn/codex-proxy-rs-version-route";
    let github = MockServer::start().await;
    mount_system_latest_release(&github, repository, SYSTEM_VERSION_NEW).await;
    let mut config = system_update_test_config(repository, &github.uri());
    config.git_sha = "version-test-sha".to_owned();
    config.build_time = "2026-07-01T00:00:00Z".to_owned();
    let (service, _) = system_update_test_service(config);

    let value = service.version().await.expect("version response");

    assert!(
        value["version"] == SYSTEM_VERSION_OLD
            && value["gitSha"] == "version-test-sha"
            && value["buildTime"] == "2026-07-01T00:00:00Z"
            && value["deploymentMode"] == "docker"
            && value["deploymentModeLabel"] == "Docker"
            && value["updateChannel"] == "stable"
            && value["latestVersion"] == SYSTEM_VERSION_NEW
            && value["hasUpdate"] == true
            && value["updateCached"] == false
            && value["updateWarning"].is_null()
    );
}

#[tokio::test]
async fn update_detail_should_use_cached_release_when_not_refreshed() {
    let repository = "zyycn/codex-proxy-rs-cache-route";
    let github = MockServer::start().await;
    mount_system_latest_release(&github, repository, "0.2.0").await;
    let (service, _) =
        system_update_test_service(system_update_test_config(repository, &github.uri()));
    let initial = service.update_detail(true).await.expect("initial release");

    github.reset().await;
    let cached = service.update_detail(false).await.expect("cached release");

    assert!(
        initial["hasUpdate"] == true
            && cached["cached"] == true
            && cached["latestVersion"] == "0.2.0"
    );
}

#[tokio::test]
async fn update_detail_should_fallback_to_cache_when_refresh_fetch_fails() {
    let repository = "zyycn/codex-proxy-rs-cache-forced-route";
    let github = MockServer::start().await;
    mount_system_latest_release(&github, repository, "0.3.0").await;
    let (service, _) =
        system_update_test_service(system_update_test_config(repository, &github.uri()));
    let initial = service.update_detail(true).await.expect("initial release");

    github.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&github)
        .await;
    let fallback = service.update_detail(true).await.expect("cached fallback");

    assert!(
        initial["hasUpdate"] == true
            && fallback["cached"] == true
            && fallback["latestVersion"] == "0.3.0"
            && fallback["warning"].is_null()
    );
}

#[tokio::test]
async fn update_detail_should_report_source_build_as_unsupported() {
    let repository = "zyycn/codex-proxy-rs-source-route";
    let github = MockServer::start().await;
    mount_system_latest_release(&github, repository, SYSTEM_VERSION_NEW).await;
    let mut config = system_update_test_config(repository, &github.uri());
    config.build_type = "source".to_owned();
    let (service, _) = system_update_test_service(config);

    let info = service.update_detail(true).await.expect("update detail");

    assert!(
        info["currentVersion"] == SYSTEM_VERSION_OLD
            && info["latestVersion"] == SYSTEM_VERSION_NEW
            && info["hasUpdate"] == true
            && info["buildType"] == "source"
            && info["buildTypeLabel"] == "源码构建"
            && info["updateSupported"] == false
            && info["unsupportedReason"] == "一键更新需要正式构建包"
    );
}

#[tokio::test]
async fn update_detail_should_reject_untrusted_github_api_base() {
    let config = system_update_test_config(
        "zyycn/codex-proxy-rs-untrusted-api-base-route",
        "https://mirror.example",
    );
    let (service, _) = system_update_test_service(config);

    let info = service.update_detail(true).await.expect("update detail");

    assert!(
        info["hasUpdate"] == false
            && info["updateSupported"] == false
            && info["unsupportedReason"] == "GitHub API base must be https://api.github.com/repos"
    );
}

#[tokio::test]
async fn restart_should_request_process_restart_inside_docker() {
    let mut config =
        system_update_test_config("zyycn/codex-proxy-rs-restart-route", "http://127.0.0.1:9");
    config.self_restart_enabled = true;
    let (service, shutdown) = system_update_test_service(config);

    let value = service.restart().await.expect("schedule docker restart");
    let cancelled = tokio::time::timeout(StdDuration::from_secs(2), shutdown.cancelled()).await;

    assert!(
        value["operationId"]
            .as_str()
            .is_some_and(|id| id.starts_with("sysop-restart-"))
            && value["message"] == "已安排进程内重启"
            && cancelled.is_ok()
    );
}

#[tokio::test]
async fn restart_should_spawn_replacement_before_shutdown_outside_docker() {
    let replacement = Path::new("/bin/true");
    if !replacement.exists() {
        return;
    }
    let mut config = system_update_test_config(
        "zyycn/codex-proxy-rs-binary-restart-route",
        "http://127.0.0.1:9",
    );
    config.deployment_mode = "binary".to_owned();
    config.self_restart_enabled = true;
    config.executable_path = Some(replacement.to_path_buf());
    let (service, shutdown) = system_update_test_service(config);

    let value = service.restart().await.expect("schedule binary restart");
    let cancelled = tokio::time::timeout(StdDuration::from_secs(2), shutdown.cancelled()).await;

    assert!(value["message"] == "已安排自重启" && cancelled.is_ok());
}

#[tokio::test]
async fn restart_should_not_shutdown_when_replacement_spawn_fails() {
    let root = tempfile::tempdir().expect("restart root");
    let mut config = system_update_test_config(
        "zyycn/codex-proxy-rs-restart-spawn-failure-route",
        "http://127.0.0.1:9",
    );
    config.deployment_mode = "binary".to_owned();
    config.self_restart_enabled = true;
    config.executable_path = Some(root.path().join("missing-binary"));
    let (service, shutdown) = system_update_test_service(config);

    let error = service.restart().await.expect_err("replacement must fail");
    let cancelled = tokio::time::timeout(StdDuration::from_millis(100), shutdown.cancelled()).await;

    assert!(
        error.kind() == SystemAdminErrorKind::Internal
            && error
                .to_string()
                .starts_with("Failed to schedule replacement process:")
            && cancelled.is_err()
    );
}

#[tokio::test]
async fn update_should_replace_local_release_files_with_latest_asset() {
    let repository = "zyycn/codex-proxy-rs-update-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    mount_system_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let (service, _) = system_update_test_service(layout.config.clone());

    let value = service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect("perform update");
    let status = service.update_status().await.expect("update status");

    assert!(
        value["operationId"]
            .as_str()
            .is_some_and(|id| id.starts_with("sysop-update-"))
            && value["targetVersion"] == "0.4.0"
            && value["needRestart"] == true
            && status["operation"]["status"] == "succeeded"
            && status["currentVersion"] == "0.4.0"
            && fs::read_to_string(&layout.executable).expect("new binary") == "new-binary"
            && fs::read_to_string(layout.web_dist.join("index.html")).expect("new web")
                == "new-web"
            && fs::read_to_string(layout.root.path().join("codex-proxy-rs.backup"))
                .expect("binary backup")
                == "old-binary"
            && fs::read_to_string(layout.root.path().join("web/dist.backup/index.html"))
                .expect("web backup")
                == "old-web"
    );
}

#[tokio::test]
async fn update_should_reject_when_confirmed_target_differs_from_remote_latest() {
    let repository = "zyycn/codex-proxy-rs-update-target-changed-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    mount_system_latest_release(&github, repository, "0.4.0").await;
    let (service, _) = system_update_test_service(layout.config.clone());
    let initial = service.update_detail(true).await.expect("initial release");
    github.reset().await;
    mount_system_latest_release(&github, repository, "0.5.0").await;

    let error = service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect_err("changed target must fail");

    assert!(
        initial["latestVersion"] == "0.4.0"
            && error.kind() == SystemAdminErrorKind::Conflict
            && error.to_string() == "远端最新版本已变更为 v0.5.0，请重新检查并确认"
            && fs::read_to_string(&layout.executable).expect("old binary") == "old-binary"
            && fs::read_to_string(layout.web_dist.join("index.html")).expect("old web")
                == "old-web"
    );
}

#[tokio::test]
async fn update_should_reject_untrusted_github_api_base() {
    let layout = SystemUpdateTestLayout::new(
        "zyycn/codex-proxy-rs-update-untrusted-api-base-route",
        "https://mirror.example",
    );
    let (service, _) = system_update_test_service(layout.config.clone());

    let error = service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect_err("untrusted API base must fail");

    assert!(
        error.kind() == SystemAdminErrorKind::Conflict
            && error.to_string() == "GitHub API base must be https://api.github.com/repos"
            && fs::read_to_string(&layout.executable).expect("old binary") == "old-binary"
            && fs::read_to_string(layout.web_dist.join("index.html")).expect("old web")
                == "old-web"
    );
}

#[tokio::test]
async fn update_should_fail_when_release_checksum_is_missing() {
    let repository = "zyycn/codex-proxy-rs-missing-checksum-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    mount_system_release_archive(
        &github,
        repository,
        "0.4.0",
        system_release_archive("new-binary", "new-web"),
        None,
    )
    .await;
    let (service, _) = system_update_test_service(layout.config.clone());

    assert_system_update_failure_preserves_files(
        service.as_ref(),
        SystemAdminErrorKind::BadGateway,
        "Release checksums.txt is required",
        &layout,
    )
    .await;
}

#[tokio::test]
async fn update_should_fail_when_release_checksum_mismatches() {
    let repository = "zyycn/codex-proxy-rs-bad-checksum-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    let archive = system_release_archive("new-binary", "new-web");
    let checksum = format!("{}  {}\n", "0".repeat(64), system_archive_name("0.4.0"));
    mount_system_release_archive(&github, repository, "0.4.0", archive, Some(checksum)).await;
    let (service, _) = system_update_test_service(layout.config.clone());

    assert_system_update_failure_preserves_files(
        service.as_ref(),
        SystemAdminErrorKind::BadGateway,
        "Checksum mismatch",
        &layout,
    )
    .await;
}

#[tokio::test]
async fn update_should_reject_release_archive_from_untrusted_host() {
    let repository = "zyycn/codex-proxy-rs-untrusted-download-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    mount_system_release_with_urls(
        &github,
        repository,
        "0.4.0",
        "https://evil.example/download/codex-proxy-rs.tar.gz",
        "https://github.com/zyycn/codex-proxy-rs/releases/download/v0.4.0/checksums.txt",
    )
    .await;
    let (service, _) = system_update_test_service(layout.config.clone());

    assert_system_update_failure_preserves_files(
        service.as_ref(),
        SystemAdminErrorKind::Invalid,
        "Download host is not allowed: evil.example",
        &layout,
    )
    .await;
}

#[tokio::test]
async fn update_should_reject_insecure_release_archive() {
    let repository = "zyycn/codex-proxy-rs-insecure-download-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    mount_system_release_with_urls(
        &github,
        repository,
        "0.4.0",
        "http://github.com/zyycn/codex-proxy-rs/releases/download/v0.4.0/archive.tar.gz",
        "https://github.com/zyycn/codex-proxy-rs/releases/download/v0.4.0/checksums.txt",
    )
    .await;
    let (service, _) = system_update_test_service(layout.config.clone());

    assert_system_update_failure_preserves_files(
        service.as_ref(),
        SystemAdminErrorKind::Invalid,
        "Only HTTPS release downloads are allowed",
        &layout,
    )
    .await;
}

#[tokio::test]
async fn update_should_reject_release_archive_with_unsafe_path() {
    let repository = "zyycn/codex-proxy-rs-unsafe-archive-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    let archive =
        system_release_archive_with_extra("new-binary", "new-web", "../escape", b"unsafe");
    let checksum = system_archive_checksum("0.4.0", &archive);
    mount_system_release_archive(&github, repository, "0.4.0", archive, Some(checksum)).await;
    let (service, _) = system_update_test_service(layout.config.clone());

    assert_system_update_failure_preserves_files(
        service.as_ref(),
        SystemAdminErrorKind::Invalid,
        "Unsafe archive path",
        &layout,
    )
    .await;
}

#[tokio::test]
async fn update_should_restore_web_assets_when_binary_backup_fails() {
    let repository = "zyycn/codex-proxy-rs-binary-backup-failure-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    fs::create_dir(layout.root.path().join("codex-proxy-rs.backup"))
        .expect("blocking backup directory");
    mount_system_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let (service, _) = system_update_test_service(layout.config.clone());

    let error = service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect_err("binary backup must fail");
    let status = service.update_status().await.expect("failed status");

    assert!(
        error.kind() == SystemAdminErrorKind::Internal
            && error
                .to_string()
                .starts_with("Failed to remove old binary backup:")
            && status["operation"]["error"]
                .as_str()
                .is_some_and(|message| message.starts_with("Failed to remove old binary backup:"))
            && fs::read_to_string(&layout.executable).expect("old binary") == "old-binary"
            && fs::read_to_string(layout.web_dist.join("index.html")).expect("old web")
                == "old-web"
            && !layout.root.path().join("web/dist.backup").exists()
    );
}

#[tokio::test]
async fn update_should_remove_stale_file_lock_and_continue() {
    let repository = "zyycn/codex-proxy-rs-stale-lock-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    fs::create_dir_all(
        layout
            .config
            .update_lock_file
            .parent()
            .expect("lock parent"),
    )
    .expect("create lock parent");
    fs::write(&layout.config.update_lock_file, "pid=1\ncreated_at=stale\n")
        .expect("write stale lock");
    fs::File::options()
        .write(true)
        .open(&layout.config.update_lock_file)
        .expect("open stale lock")
        .set_modified(SystemTime::now() - StdDuration::from_secs(31 * 60))
        .expect("age stale lock");
    mount_system_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let (service, _) = system_update_test_service(layout.config.clone());

    service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect("update past stale lock");

    assert!(
        fs::read_to_string(&layout.executable).expect("new binary") == "new-binary"
            && fs::read_to_string(layout.web_dist.join("index.html")).expect("new web")
                == "new-web"
            && !layout.config.update_lock_file.exists()
    );
}

#[tokio::test]
async fn update_should_replace_web_assets_across_filesystems() {
    let Ok(web_root) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    let repository = "zyycn/codex-proxy-rs-cross-device-update-route";
    let github = MockServer::start().await;
    let mut layout = SystemUpdateTestLayout::new(repository, &github.uri());
    let web_dist = web_root.path().join("web/dist");
    fs::create_dir_all(&web_dist).expect("cross-device web directory");
    fs::write(web_dist.join("index.html"), "old-web").expect("cross-device old web");
    layout.config.web_dist_dir = web_dist.clone();
    layout.web_dist = web_dist.clone();
    mount_system_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let (service, _) = system_update_test_service(layout.config.clone());

    service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect("cross-device update");

    assert!(
        fs::read_to_string(&layout.executable).expect("new binary") == "new-binary"
            && fs::read_to_string(web_dist.join("index.html")).expect("new web") == "new-web"
            && fs::read_to_string(web_root.path().join("web/dist.backup/index.html"))
                .expect("old web backup")
                == "old-web"
    );
}

#[tokio::test]
async fn update_status_should_read_local_update_state() {
    let layout =
        SystemUpdateTestLayout::new("zyycn/codex-proxy-rs-status-route", "http://127.0.0.1:9");
    fs::create_dir_all(
        layout
            .config
            .update_state_file
            .parent()
            .expect("state parent"),
    )
    .expect("create state parent");
    fs::write(
        &layout.config.update_state_file,
        json!({
            "previousVersion": SYSTEM_VERSION_OLD,
            "currentVersion": SYSTEM_VERSION_NEW,
            "operation": {
                "operationId": "system-update-status",
                "kind": "update",
                "status": "running",
                "targetVersion": SYSTEM_VERSION_NEW,
                "message": "operation running",
                "error": null,
                "startedAt": "2026-07-01T00:00:00Z",
                "finishedAt": null
            }
        })
        .to_string(),
    )
    .expect("write update state");
    let (service, _) = system_update_test_service(layout.config.clone());

    let status = service.update_status().await.expect("update status");

    assert!(
        status["operation"]["operationId"] == "system-update-status"
            && status["operation"]["status"] == "running"
            && status["currentVersion"] == SYSTEM_VERSION_NEW
    );
}

#[tokio::test]
async fn update_events_should_open_authenticated_sse_stream() {
    let layout =
        SystemUpdateTestLayout::new("zyycn/codex-proxy-rs-events-route", "http://127.0.0.1:9");
    let (service, _) = system_update_test_service(layout.config.clone());
    let app = system_update_test_router(service);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/system/update-events")
                .header("x-request-id", "req-system-events-denied")
                .body(Body::empty())
                .expect("unauthorized request"),
        )
        .await
        .expect("unauthorized response");
    let response = app
        .oneshot(system_update_authenticated_request(
            "GET",
            "/api/admin/system/update-events",
            Body::empty(),
        ))
        .await
        .expect("SSE response");
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    assert!(
        unauthorized.status() == StatusCode::UNAUTHORIZED
            && response.status() == StatusCode::OK
            && content_type.starts_with("text/event-stream")
    );
}

#[tokio::test]
async fn update_events_should_close_after_terminal_update_log() {
    let repository = "zyycn/codex-proxy-rs-events-terminal-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    mount_system_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let (service, _) = system_update_test_service(layout.config.clone());
    let app = system_update_test_router(Arc::clone(&service));
    let response = app
        .oneshot(system_update_authenticated_request(
            "GET",
            "/api/admin/system/update-events",
            Body::empty(),
        ))
        .await
        .expect("SSE response");
    let update_service = Arc::clone(&service);
    let update = tokio::spawn(async move {
        update_service
            .perform_update(Some("0.4.0".to_owned()))
            .await
    });

    let body = tokio::time::timeout(
        StdDuration::from_secs(2),
        to_bytes(response.into_body(), 128 * 1024),
    )
    .await
    .expect("terminal event should close SSE")
    .expect("read SSE body");
    update
        .await
        .expect("join update")
        .expect("perform terminal update");
    let text = String::from_utf8(body.to_vec()).expect("UTF-8 SSE");

    assert!(
        text.contains("\"step\":\"done\"")
            && text.contains("\"terminal\":true")
            && text.contains("更新文件已替换，等待服务重启生效")
    );
}

#[tokio::test]
async fn rollback_should_restore_binary_web_and_version_state() {
    let repository = "zyycn/codex-proxy-rs-rollback-route";
    let github = MockServer::start().await;
    let layout = SystemUpdateTestLayout::new(repository, &github.uri());
    mount_system_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let (service, _) = system_update_test_service(layout.config.clone());
    service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect("perform update before rollback");

    let value = service.rollback().await.expect("rollback release");
    let status = service.update_status().await.expect("rollback status");

    assert!(
        value["needRestart"] == true
            && value["operationId"]
                .as_str()
                .is_some_and(|id| id.starts_with("sysop-rollback-"))
            && status["operation"]["status"] == "succeeded"
            && status["currentVersion"] == SYSTEM_VERSION_OLD
            && status["previousVersion"] == "0.4.0"
            && fs::read_to_string(&layout.executable).expect("rolled-back binary") == "old-binary"
            && fs::read_to_string(layout.web_dist.join("index.html")).expect("rolled-back web")
                == "old-web"
            && fs::read_to_string(layout.root.path().join("codex-proxy-rs.backup"))
                .expect("new binary backup")
                == "new-binary"
    );
}

struct SystemUpdateTestLayout {
    root: tempfile::TempDir,
    executable: PathBuf,
    web_dist: PathBuf,
    config: SystemUpdateConfig,
}

impl SystemUpdateTestLayout {
    fn new(repository: &str, github_api_base: &str) -> Self {
        let root = tempfile::tempdir().expect("system update root");
        let executable = root.path().join("codex-proxy-rs");
        let web_dist = root.path().join("web/dist");
        fs::create_dir_all(&web_dist).expect("create old web directory");
        fs::write(&executable, "old-binary").expect("write old binary");
        fs::write(web_dist.join("index.html"), "old-web").expect("write old web");
        let mut config = system_update_test_config(repository, github_api_base);
        config.executable_path = Some(executable.clone());
        config.web_dist_dir = web_dist.clone();
        config.update_state_file = root.path().join(".runtime/update-state.json");
        config.update_lock_file = root.path().join(".runtime/update.lock");
        config.update_temp_dir = root.path().join(".runtime/update-tmp");
        Self {
            root,
            executable,
            web_dist,
            config,
        }
    }
}

fn system_update_test_config(repository: &str, github_api_base: &str) -> SystemUpdateConfig {
    SystemUpdateConfig {
        version: SYSTEM_VERSION_OLD.to_owned(),
        git_sha: "test-git-sha".to_owned(),
        build_time: "2026-07-01T00:00:00Z".to_owned(),
        deployment_mode: "docker".to_owned(),
        build_type: "release".to_owned(),
        update_channel: "stable".to_owned(),
        update_repository: Some(repository.to_owned()),
        github_api_base: format!("{github_api_base}/repos"),
        executable_path: Some(PathBuf::from("/app/bin/codex-proxy-rs")),
        web_dist_dir: PathBuf::from("/app/web/dist"),
        update_state_file: PathBuf::from("/app/.runtime/data/update-state.json"),
        update_lock_file: PathBuf::from("/app/.runtime/data/update.lock"),
        update_temp_dir: PathBuf::from("/app/.runtime/data/update-tmp"),
        self_restart_enabled: false,
    }
}

fn system_update_test_service(
    config: SystemUpdateConfig,
) -> (Arc<ProcessSystemAdminService>, CancellationToken) {
    let shutdown = CancellationToken::new();
    (
        Arc::new(ProcessSystemAdminService::with_config(
            shutdown.clone(),
            config,
        )),
        shutdown,
    )
}

async fn assert_system_update_failure_preserves_files(
    service: &ProcessSystemAdminService,
    expected_kind: SystemAdminErrorKind,
    expected_message: &str,
    layout: &SystemUpdateTestLayout,
) {
    let error = service
        .perform_update(Some("0.4.0".to_owned()))
        .await
        .expect_err("update must fail");
    let status = service.update_status().await.expect("failed update status");
    assert!(
        error.kind() == expected_kind
            && error.to_string() == expected_message
            && status["operation"]["status"] == "failed"
            && status["operation"]["error"] == expected_message
            && fs::read_to_string(&layout.executable).expect("old binary") == "old-binary"
            && fs::read_to_string(layout.web_dist.join("index.html")).expect("old web")
                == "old-web"
    );
}

async fn mount_system_latest_release(server: &MockServer, repository: &str, version: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": format!("v{version}"),
            "name": format!("v{version}"),
            "body": "## Changes\n- Update package",
            "html_url": format!("https://github.com/{repository}/releases/tag/v{version}"),
            "prerelease": false,
            "assets": []
        })))
        .mount(server)
        .await;
}

async fn mount_system_release_with_archive(
    server: &MockServer,
    repository: &str,
    version: &str,
    binary: &str,
    index: &str,
) {
    let archive = system_release_archive(binary, index);
    let checksum = system_archive_checksum(version, &archive);
    mount_system_release_archive(server, repository, version, archive, Some(checksum)).await;
}

async fn mount_system_release_archive(
    server: &MockServer,
    repository: &str,
    version: &str,
    archive: Vec<u8>,
    checksum: Option<String>,
) {
    let archive_name = system_archive_name(version);
    let mut assets = vec![json!({
        "name": archive_name,
        "browser_download_url": format!("{}/download/{archive_name}", server.uri()),
        "size": archive.len()
    })];
    if let Some(checksum) = checksum.as_ref() {
        assets.push(json!({
            "name": "checksums.txt",
            "browser_download_url": format!("{}/download/checksums.txt", server.uri()),
            "size": checksum.len()
        }));
    }
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": format!("v{version}"),
            "name": format!("v{version}"),
            "body": "## Changes\n- Update package",
            "html_url": format!("https://github.com/{repository}/releases/tag/v{version}"),
            "prerelease": false,
            "assets": assets
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/download/{archive_name}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .mount(server)
        .await;
    if let Some(checksum) = checksum {
        Mock::given(method("GET"))
            .and(path("/download/checksums.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum))
            .mount(server)
            .await;
    }
}

async fn mount_system_release_with_urls(
    server: &MockServer,
    repository: &str,
    version: &str,
    archive_url: &str,
    checksum_url: &str,
) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": format!("v{version}"),
            "name": format!("v{version}"),
            "body": "## Changes\n- Update package",
            "html_url": format!("https://github.com/{repository}/releases/tag/v{version}"),
            "prerelease": false,
            "assets": [
                {
                    "name": system_archive_name(version),
                    "browser_download_url": archive_url,
                    "size": 128
                },
                {
                    "name": "checksums.txt",
                    "browser_download_url": checksum_url,
                    "size": 128
                }
            ]
        })))
        .mount(server)
        .await;
}

fn system_archive_name(version: &str) -> String {
    format!(
        "codex-proxy-rs_{version}_{}_{}.tar.gz",
        std::env::consts::OS,
        system_release_arch()
    )
}

fn system_release_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        arch => arch,
    }
}

fn system_archive_checksum(version: &str, archive: &[u8]) -> String {
    format!(
        "{}  {}\n",
        hex::encode(Sha256::digest(archive)),
        system_archive_name(version)
    )
}

fn system_release_archive(binary: &str, index: &str) -> Vec<u8> {
    system_release_archive_with_extra(binary, index, "", &[])
}

fn system_release_archive_with_extra(
    binary: &str,
    index: &str,
    extra_path: &str,
    extra_bytes: &[u8],
) -> Vec<u8> {
    let mut data = Vec::new();
    {
        let encoder = GzEncoder::new(&mut data, Compression::default());
        let mut builder = Builder::new(encoder);
        append_system_tar_file(&mut builder, "codex-proxy-rs", binary.as_bytes());
        append_system_tar_file(&mut builder, "web/dist/index.html", index.as_bytes());
        if !extra_path.is_empty() {
            append_system_raw_tar_file(&mut builder, extra_path, extra_bytes);
        }
        builder.finish().expect("finish release archive");
    }
    data
}

fn append_system_tar_file<W: Write>(builder: &mut Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, bytes)
        .expect("append release file");
}

fn append_system_raw_tar_file<W: Write>(builder: &mut Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_old();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(EntryType::Regular);
    let name = path.as_bytes();
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_cksum();
    builder
        .append(&header, bytes)
        .expect("append raw release file");
}

#[derive(Clone)]
struct SystemUpdateTestState {
    service: Arc<ProcessSystemAdminService>,
    auth: Arc<SystemUpdateTestAuth>,
}

struct SystemUpdateTestAuth;

#[async_trait]
impl AdminSessionResolver for SystemUpdateTestAuth {
    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminServiceError> {
        Ok((session_id == Some("session_1")).then(|| "admin_1".to_owned()))
    }
}

impl AdminSessionState for SystemUpdateTestState {
    fn admin_session_resolver(&self) -> &dyn AdminSessionResolver {
        self.auth.as_ref()
    }
}

impl SystemAdminState for SystemUpdateTestState {
    fn system_admin_service(&self) -> &dyn SystemAdminService {
        self.service.as_ref()
    }
}

fn system_update_test_router(service: Arc<ProcessSystemAdminService>) -> axum::Router {
    system::router::<SystemUpdateTestState>().with_state(SystemUpdateTestState {
        service,
        auth: Arc::new(SystemUpdateTestAuth),
    })
}

fn system_update_authenticated_request(method: &str, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", "cpr_admin_session=session_1")
        .header("x-request-id", "req-system-update")
        .body(body)
        .expect("authenticated system request")
}
