use std::time::Instant;

use reqwest::StatusCode;
use serde::Serialize;

use crate::{
    codex::{
        fingerprint::model::Fingerprint,
        transport::client::{
            build_reqwest_client, CodexBackendClient, CodexClientError, CodexRequestContext,
        },
    },
    config::AppConfig,
};

#[derive(Clone)]
pub struct DiagnosticsService {
    config: AppConfig,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamProbeResult {
    pub target: &'static str,
    pub status: &'static str,
    pub backend_base_url: String,
    pub endpoint: Option<String>,
    pub reachable: bool,
    pub status_code: Option<u16>,
    pub authorization: &'static str,
    pub duration_ms: u64,
    pub error: Option<String>,
}

impl DiagnosticsService {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn probe_upstream(&self, request_id: &str) -> UpstreamProbeResult {
        let started_at = Instant::now();
        let fingerprint = Fingerprint::default_codex_desktop();
        let client = match build_reqwest_client(self.config.tls.force_http11) {
            Ok(client) => {
                CodexBackendClient::new(client, self.config.api.base_url.clone(), fingerprint)
            }
            Err(error) => {
                return self.unreachable_probe(
                    None,
                    started_at,
                    format!("failed to build HTTP client: {error}"),
                );
            }
        };

        match client
            .probe_models_endpoint(CodexRequestContext {
                access_token: "diagnostic-probe",
                account_id: None,
                request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: None,
            })
            .await
        {
            Ok(probe) => self.reachable_probe(probe.endpoint, probe.status, started_at),
            Err(error) => self.unreachable_probe(None, started_at, public_probe_error(&error)),
        }
    }

    fn reachable_probe(
        &self,
        endpoint: String,
        status: StatusCode,
        started_at: Instant,
    ) -> UpstreamProbeResult {
        UpstreamProbeResult {
            target: "codexModels",
            status: "reachable",
            backend_base_url: self.config.api.base_url.clone(),
            endpoint: Some(endpoint),
            reachable: true,
            status_code: Some(status.as_u16()),
            authorization: authorization_status(status),
            duration_ms: elapsed_millis(started_at),
            error: None,
        }
    }

    fn unreachable_probe(
        &self,
        endpoint: Option<String>,
        started_at: Instant,
        error: String,
    ) -> UpstreamProbeResult {
        UpstreamProbeResult {
            target: "codexModels",
            status: "unreachable",
            backend_base_url: self.config.api.base_url.clone(),
            endpoint,
            reachable: false,
            status_code: None,
            authorization: "unknown",
            duration_ms: elapsed_millis(started_at),
            error: Some(error),
        }
    }
}

fn authorization_status(status: StatusCode) -> &'static str {
    if status.is_success() {
        "accepted"
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        "rejected"
    } else {
        "unknown"
    }
}

fn elapsed_millis(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn public_probe_error(error: &CodexClientError) -> String {
    error.to_string().chars().take(200).collect()
}
