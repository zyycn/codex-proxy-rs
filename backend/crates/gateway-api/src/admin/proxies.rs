//! 账号出站代理管理的 HTTP 请求校验、路由与展示投影

use crate::auth::SessionState;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use gateway_admin::model::{
    PageSize, Revision,
    proxies::{
        NewProxy, ProxyAccountListQuery, ProxyListQuery, ProxyMutation, ProxyRecord,
        ProxyTestResult, UpdateProxy,
    },
};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse, PageMeta,
    accounts::{AccountGroupRefView, AccountProxyUpdate},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListQuery {
    page: Option<u32>,
    page_size: Option<u16>,
    search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccountsQuery {
    proxy_id: String,
    page: Option<u32>,
    page_size: Option<u16>,
    search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveAccountRequest {
    proxy_id: String,
    account_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRequest {
    #[serde(default)]
    auto_location: bool,
    location: Option<gateway_core::account::RequestLocation>,
    name: String,
    proxy_url: AccountProxyUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateRequest {
    auto_location: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_location_update")]
    location: Option<Option<gateway_core::account::RequestLocation>>,
    id: String,
    revision: u64,
    name: String,
    proxy_url: Option<AccountProxyUpdate>,
}

fn deserialize_location_update<'de, D>(
    deserializer: D,
) -> Result<Option<Option<gateway_core::account::RequestLocation>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdRequest {
    id: String,
    revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestRequest {
    id: String,
    revision: u64,
    #[serde(default)]
    detect_location: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeRequest {
    #[serde(default)]
    detect_location: bool,
    proxy_url: AccountProxyUpdate,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestView {
    location: gateway_admin::model::proxies::ProxyLocationDetection,
    success: bool,
    latency_ms: u64,
    exit_ip: Option<String>,
    exit_ipv4: Option<String>,
    exit_ipv6: Option<String>,
    message: String,
}

impl From<ProxyTestResult> for ProxyTestView {
    fn from(result: ProxyTestResult) -> Self {
        Self {
            location: result.location,
            success: result.success,
            latency_ms: result.latency_ms,
            exit_ip: result.exit_ip.map(|ip| ip.to_string()),
            exit_ipv4: result.exit_ipv4.map(|ip| ip.to_string()),
            exit_ipv6: result.exit_ipv6.map(|ip| ip.to_string()),
            message: result.message,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyAccountView {
    id: String,
    name: String,
    email: Option<String>,
    provider: String,
    authentication_kind: String,
    plan_type: Option<String>,
    plan_type_display: Option<String>,
    groups: Vec<AccountGroupRefView>,
    enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyView {
    auto_location: bool,
    detected_location: Option<gateway_admin::model::proxies::DetectedProxyLocation>,
    location: Option<gateway_core::account::RequestLocation>,
    id: String,
    name: String,
    endpoint: String,
    has_authentication: bool,
    revision: u64,
    account_count: u64,
    last_test_at: Option<String>,
    last_test_at_display: Option<String>,
    last_test: Option<ProxyTestView>,
    created_at: String,
    created_at_display: String,
    updated_at: String,
    updated_at_display: String,
}

impl From<(ProxyRecord, crate::time::TimePresenter)> for ProxyView {
    fn from((record, time): (ProxyRecord, crate::time::TimePresenter)) -> Self {
        let endpoint = record.proxy.endpoint();
        Self {
            auto_location: record.auto_location,
            detected_location: record.detected_location,
            location: record.location,
            id: record.id,
            name: record.name,
            has_authentication: record.proxy.expose_url() != endpoint,
            endpoint,
            revision: record.revision.get(),
            account_count: record.account_count,
            last_test_at_display: record
                .last_test_at
                .as_ref()
                .map(|value| time.datetime(value)),
            last_test_at: record.last_test_at.map(|at| at.to_rfc3339()),
            last_test: record.last_test.map(Into::into),
            created_at_display: time.datetime(&record.created_at),
            created_at: record.created_at.to_rfc3339(),
            updated_at_display: time.datetime(&record.updated_at),
            updated_at: record.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Serialize)]
struct ProxyPageView {
    items: Vec<ProxyView>,
    page: PageMeta,
}

#[derive(Serialize)]
struct ProxyAccountPageView {
    items: Vec<ProxyAccountView>,
    page: PageMeta,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationView {
    record: ProxyView,
    config_revision: u64,
}

impl From<(ProxyMutation, crate::time::TimePresenter)> for MutationView {
    fn from((value, time): (ProxyMutation, crate::time::TimePresenter)) -> Self {
        Self {
            record: ProxyView::from((value.record, time)),
            config_revision: value.config_revision.get(),
        }
    }
}

pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/proxies", get(list::<S>))
        .route("/api/admin/proxies/accounts", get(list_accounts::<S>))
        .route(
            "/api/admin/proxies/accounts/remove",
            post(remove_account::<S>),
        )
        .route("/api/admin/proxies/create", post(create::<S>))
        .route("/api/admin/proxies/update", post(update::<S>))
        .route("/api/admin/proxies/delete", post(delete::<S>))
        .route("/api/admin/proxies/test", post(test::<S>))
        .route("/api/admin/proxies/probe", post(probe::<S>))
}

fn revision(value: u64) -> Result<Revision, AdminError> {
    if value > i64::MAX as u64 {
        return Err(AdminError::bad_request("代理版本不合法"));
    }
    Revision::new(value).map_err(|_| AdminError::bad_request("代理版本不合法"))
}

fn map_error(error: gateway_admin::model::AdminError) -> AdminError {
    super::wire::map_admin_service_error(error)
}

async fn list<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ListQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let result = state
        .admin_services()
        .proxies()
        .list(ProxyListQuery {
            page: query.page.unwrap_or(1),
            page_size: PageSize::new(query.page_size.unwrap_or(20))
                .map_err(|_| AdminError::bad_request("分页大小不合法"))?,
            search: query.search.unwrap_or_default().trim().to_owned(),
        })
        .await
        .map_err(map_error)?;
    let total_pages = result.total.div_ceil(u64::from(result.page_size));
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyPageView {
            items: result
                .items
                .into_iter()
                .map(|value| ProxyView::from((value, time)))
                .collect(),
            page: PageMeta::new(
                result.page,
                u32::from(result.page_size),
                result.total,
                u32::try_from(total_pages).unwrap_or(u32::MAX),
            ),
        }),
    ))
}

async fn list_accounts<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .list_accounts(ProxyAccountListQuery {
            proxy_id: query.proxy_id,
            page: query.page.unwrap_or(1),
            page_size: PageSize::new(query.page_size.unwrap_or(20))
                .map_err(|_| AdminError::bad_request("分页大小不合法"))?,
            search: query.search.unwrap_or_default().trim().to_owned(),
        })
        .await
        .map_err(map_error)?;
    let total_pages = result.total.div_ceil(u64::from(result.page_size));
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyAccountPageView {
            items: result
                .items
                .into_iter()
                .map(|account| ProxyAccountView {
                    id: account.id,
                    name: account.name,
                    email: account.email,
                    provider: account.provider_kind,
                    authentication_kind: account.authentication_kind,
                    plan_type: account.plan_type,
                    plan_type_display: account.plan_type_display,
                    groups: account
                        .groups
                        .into_iter()
                        .map(|group| AccountGroupRefView {
                            id: group.id.to_string(),
                            name: group.name,
                            color: group.color.as_str().to_owned(),
                            enabled: group.enabled,
                        })
                        .collect(),
                    enabled: account.enabled,
                })
                .collect(),
            page: PageMeta::new(
                result.page,
                u32::from(result.page_size),
                result.total,
                u32::try_from(total_pages).unwrap_or(u32::MAX),
            ),
        }),
    ))
}

async fn create<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<CreateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let proxy = request
        .proxy_url
        .0
        .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))?;
    let result = state
        .admin_services()
        .proxies()
        .create(
            NewProxy {
                auto_location: request.auto_location,
                test: None,
                location: request.location,
                name: request.name,
                proxy,
            },
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(MutationView::from((result, time))),
    ))
}

async fn remove_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RemoveAccountRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let revision = state
        .admin_services()
        .proxies()
        .remove_account(
            &request.proxy_id,
            &request.account_id,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision": revision.get()})),
    ))
}

async fn update<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let proxy = request
        .proxy_url
        .map(|value| {
            value
                .0
                .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))
        })
        .transpose()?;
    let result = state
        .admin_services()
        .proxies()
        .update(
            UpdateProxy {
                auto_location: request.auto_location,
                test: None,
                location: request.location,
                id: request.id,
                revision: revision(request.revision)?,
                name: request.name,
                proxy,
            },
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(MutationView::from((result, time))),
    ))
}

async fn delete<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<IdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .delete(
            &request.id,
            revision(request.revision)?,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({"configRevision": result.get()})),
    ))
}

async fn probe<S>(
    _: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ProbeRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let proxy = request
        .proxy_url
        .0
        .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))?;
    let result = state
        .admin_services()
        .proxies()
        .probe(&proxy, request.detect_location)
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyTestView::from(result)),
    ))
}

async fn test<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<TestRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let result = state
        .admin_services()
        .proxies()
        .test(
            &request.id,
            revision(request.revision)?,
            request.detect_location,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyView::from((result, time))),
    ))
}
