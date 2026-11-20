//! 管理员查看单个密钥的模型统计与手动对齐本期额度的接口

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use chrono::{DateTime, Utc};
use gateway_admin::model::observability::{KeyStatisticsOverview, KeyUsageMetric, TimeRange};
use gateway_admin::{
    AdminServices,
    model::client_keys::{
        ClientKeyBudgetMutationOrigin, ClientKeyBudgetReference, ClientKeyBudgetWindowPeriod,
        SetClientKeyBudgetWindow,
    },
};
use gateway_core::{account::ProviderAccountId, policy::ClientApiKeyId};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse,
    client_keys::ClientBudgetView, wire::map_admin_service_error,
};
use crate::{auth::SessionState, time::TimePresenter};

pub(super) fn router<S: SessionState + Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            "/api/admin/client-keys/budget-reference-accounts",
            get(accounts::<S>),
        )
        .route("/api/admin/client-keys/statistics", get(statistics::<S>))
        .route("/api/admin/client-keys/statistics/models", get(models::<S>))
        .route(
            "/api/admin/client-keys/budget-alignment",
            get(preview::<S>).post(align::<S>),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatisticsQuery {
    id: String,
    period: Option<String>,
    #[serde(default)]
    metric: KeyUsageMetric,
    as_of: Option<i64>,
}

fn key_id(value: String) -> Result<ClientApiKeyId, AdminError> {
    ClientApiKeyId::new(value).map_err(|_| AdminError::bad_request("API Key ID 无效"))
}

#[derive(Serialize)]
struct Statistics {
    budget: ClientBudgetView,
    dates: Vec<chrono::NaiveDate>,
    #[serde(rename = "asOf")]
    as_of: i64,
    #[serde(flatten)]
    overview: KeyStatisticsOverview,
}

async fn statistics<S: SessionState>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<StatisticsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let services = state.admin_services();
    let id = key_id(query.id.clone())?;
    let tz = services.timezone();
    let range = statistics_range(&query, tz)?;
    let budget = services
        .client_keys()
        .budget(&id)
        .await
        .map_err(map_admin_service_error)?;
    let overview = services
        .observability()
        .client_key_statistics(range, &id, query.metric)
        .await
        .map_err(map_admin_service_error)?;
    let end = tz.local(range.end).date_naive();
    let dates = tz
        .local(range.start)
        .date_naive()
        .iter_days()
        .take_while(|date| *date <= end)
        .collect();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(Statistics {
            budget: ClientBudgetView::from((budget, TimePresenter::new(tz))),
            dates,
            as_of: range.end.timestamp_millis(),
            overview,
        }),
    ))
}

fn statistics_range(
    query: &StatisticsQuery,
    tz: gateway_core::time::DeploymentTimeZone,
) -> Result<TimeRange, AdminError> {
    let period = query.period.as_deref().unwrap_or("7d");
    if !matches!(period, "7d" | "30d") {
        return Err(AdminError::bad_request("统计周期须为 7 天或 30 天"));
    }
    crate::time::query_range(
        None,
        None,
        Some(period),
        Some(query.as_of.unwrap_or_else(|| Utc::now().timestamp_millis())),
        tz,
        "7d",
    )
    .map_err(|_| AdminError::bad_request("统计时间范围无效"))
}

async fn models<S: SessionState>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<StatisticsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let services = state.admin_services();
    let range = statistics_range(&query, services.timezone())?;
    let id = key_id(query.id)?;
    services
        .client_keys()
        .budget(&id)
        .await
        .map_err(map_admin_service_error)?;
    let items = services
        .observability()
        .client_key_models(range, &id)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(items)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountSearch {
    id: String,
    #[serde(default)]
    search: String,
    #[serde(default)]
    cursor: String,
}

async fn accounts<S: SessionState>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountSearch>,
) -> Result<impl IntoResponse, AdminError> {
    let page = state
        .admin_services()
        .client_keys()
        .budget_reference_accounts(&key_id(query.id)?, &query.search, &query.cursor)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(page)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReferenceQuery {
    id: String,
    account_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceWindow {
    key: String,
    label: String,
    period: &'static str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    starts_at_display: String,
    ends_at_display: String,
}

#[derive(Serialize)]
struct Preview {
    revision: u64,
    windows: Vec<ReferenceWindow>,
}

async fn reference_windows(
    services: &AdminServices,
    id: &ClientApiKeyId,
    account: String,
) -> Result<Vec<ClientKeyBudgetReference>, AdminError> {
    let account_id =
        ProviderAccountId::new(account).map_err(|_| AdminError::bad_request("参考账号 ID 无效"))?;
    let key = services
        .client_keys()
        .get(id)
        .await
        .map_err(map_admin_service_error)?;
    services
        .accounts()
        .key_budget_references(&account_id, &key.groups)
        .await
        .map_err(map_admin_service_error)
}

async fn preview<S: SessionState>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ReferenceQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let services = state.admin_services();
    let id = key_id(query.id)?;
    let time = TimePresenter::new(services.timezone());
    let windows = reference_windows(services, &id, query.account_id)
        .await?
        .into_iter()
        .map(
            |ClientKeyBudgetReference {
                 key,
                 label,
                 period,
                 bounds,
             }| ReferenceWindow {
                key,
                label,
                period: period.as_str(),
                starts_at: bounds.starts_at,
                ends_at: bounds.ends_at,
                starts_at_display: time.datetime(&bounds.starts_at),
                ends_at_display: time.datetime(&bounds.ends_at),
            },
        )
        .collect();
    // 日周窗口共享配置版本，普通请求的消费累计不会让预览失效
    let revision = services
        .client_keys()
        .budget_window(&id, ClientKeyBudgetWindowPeriod::Weekly)
        .await
        .map_err(map_admin_service_error)?
        .revision;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(Preview { revision, windows }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AlignRequest {
    id: String,
    account_id: String,
    window_key: String,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    revision: u64,
}

async fn align<S: SessionState>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(input): AdminJson<AlignRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let services = state.admin_services();
    let id = key_id(input.id)?;
    let reference = reference_windows(services, &id, input.account_id)
        .await?
        .into_iter()
        .find(|reference| {
            reference.key == input.window_key
                && reference.bounds.starts_at == input.starts_at
                && reference.bounds.ends_at == input.ends_at
        })
        .ok_or_else(|| AdminError::conflict("参考窗口已变化，请重新预览"))?;
    let revision = services
        .client_keys()
        .set_budget_window(
            &auth.context().mutation_context(),
            SetClientKeyBudgetWindow {
                id,
                period: reference.period,
                expected_revision: input.revision,
                window: Some(reference.bounds),
            },
            ClientKeyBudgetMutationOrigin::Admin,
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(revision),
    ))
}
