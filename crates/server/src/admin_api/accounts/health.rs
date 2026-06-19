//! 账号健康检查处理器。

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_runtime::{
    services::{AdminAccountProbeOutcome, AdminAccountProbeResult},
    state::AppState,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin_api::{
        accounts::{account_error, account_status_value},
        require_admin_session, AdminEnvelope, AdminError, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 健康检查请求。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HealthCheckRequest {
    /// 指定账号 ID；为空时检查全部账号。
    pub ids: Option<Vec<String>>,
    /// 每个探测启动间隔毫秒。
    pub stagger_ms: Option<u64>,
    /// 并发数量。
    pub concurrency: Option<u8>,
}

/// 健康检查响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckData {
    /// 汇总。
    pub summary: HealthCheckSummary,
    /// 逐账号结果。
    pub results: Vec<AccountProbeData>,
}

/// 健康检查汇总。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckSummary {
    /// 总数。
    pub total: usize,
    /// 可用数。
    pub alive: usize,
    /// 不可用数。
    pub dead: usize,
    /// 跳过数。
    pub skipped: usize,
}

/// 单账号探测结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProbeData {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 探测前状态。
    pub previous_status: String,
    /// 探测结果。
    pub result: String,
    /// 探测后状态。
    pub status: Option<String>,
    /// 错误信息。
    pub error: Option<String>,
    /// 耗时毫秒。
    pub duration_ms: Option<u128>,
}

/// `POST /api/admin/accounts/health-check`
pub async fn health_check_accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    let payload = parse_health_check_request(&body, &request_id)?;
    validate_health_check_request(&payload, &request_id)?;
    require_admin_session(&state, &headers, &request_id).await?;

    let concurrency = usize::from(payload.concurrency.unwrap_or(2));
    let stagger_ms = payload.stagger_ms.unwrap_or(3_000);
    match state
        .services
        .admin_accounts
        .health_check_accounts(payload.ids, concurrency, stagger_ms, &request_id)
        .await
    {
        Ok(results) => {
            let results = results
                .into_iter()
                .map(AccountProbeData::from)
                .collect::<Vec<_>>();
            let summary = HealthCheckSummary {
                total: results.len(),
                alive: results
                    .iter()
                    .filter(|result| result.result == "alive")
                    .count(),
                dead: results
                    .iter()
                    .filter(|result| result.result == "dead")
                    .count(),
                skipped: results
                    .iter()
                    .filter(|result| result.result == "skipped")
                    .count(),
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(HealthCheckData { summary, results }, request_id),
            ))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}

fn parse_health_check_request(
    body: &Bytes,
    request_id: &str,
) -> Result<HealthCheckRequest, AdminError> {
    if body.is_empty() {
        return Ok(HealthCheckRequest::default());
    }
    serde_json::from_slice(body).map_err(|_| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Invalid health check request",
            request_id,
        )
    })
}

fn validate_health_check_request(
    payload: &HealthCheckRequest,
    request_id: &str,
) -> Result<(), AdminError> {
    if payload.ids.as_ref().is_some_and(Vec::is_empty) {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids must not be empty",
            request_id,
        ));
    }
    if payload
        .stagger_ms
        .is_some_and(|value| !(500..=30_000).contains(&value))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "staggerMs must be between 500 and 30000",
            request_id,
        ));
    }
    if payload
        .concurrency
        .is_some_and(|value| !(1..=10).contains(&value))
    {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "concurrency must be between 1 and 10",
            request_id,
        ));
    }
    Ok(())
}

impl From<AdminAccountProbeResult> for AccountProbeData {
    fn from(result: AdminAccountProbeResult) -> Self {
        Self {
            id: result.id,
            email: result.email,
            previous_status: account_status_value(result.previous_status).to_string(),
            result: probe_outcome_value(result.outcome).to_string(),
            status: result
                .status
                .map(account_status_value)
                .map(ToString::to_string),
            error: result.error,
            duration_ms: result.duration_ms,
        }
    }
}

fn probe_outcome_value(outcome: AdminAccountProbeOutcome) -> &'static str {
    outcome.as_str()
}
