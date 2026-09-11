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
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse, AdminSessionState,
    PageMeta,
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
    name: String,
    proxy_url: AccountProxyUpdate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateRequest {
    id: String,
    revision: u64,
    name: String,
    proxy_url: Option<AccountProxyUpdate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdRequest {
    id: String,
    revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProbeRequest {
    proxy_url: AccountProxyUpdate,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestView {
    success: bool,
    latency_ms: u64,
    exit_ip: Option<String>,
    message: String,
}

impl From<ProxyTestResult> for ProxyTestView {
    fn from(result: ProxyTestResult) -> Self {
        Self {
            success: result.success,
            latency_ms: result.latency_ms,
            exit_ip: result.exit_ip.map(|ip| ip.to_string()),
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
    id: String,
    name: String,
    endpoint: String,
    has_authentication: bool,
    revision: u64,
    account_count: u64,
    last_test_at: Option<String>,
    last_test: Option<ProxyTestView>,
    created_at: String,
    updated_at: String,
}

impl From<ProxyRecord> for ProxyView {
    fn from(record: ProxyRecord) -> Self {
        let endpoint = record.proxy.endpoint();
        Self {
            id: record.id,
            name: record.name,
            has_authentication: record.proxy.expose_url() != endpoint,
            endpoint,
            revision: record.revision.get(),
            account_count: record.account_count,
            last_test_at: record.last_test_at.map(|at| at.to_rfc3339()),
            last_test: record.last_test.map(Into::into),
            created_at: record.created_at.to_rfc3339(),
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

impl From<ProxyMutation> for MutationView {
    fn from(value: ProxyMutation) -> Self {
        Self {
            record: value.record.into(),
            config_revision: value.config_revision.get(),
        }
    }
}

pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
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
    S: AdminSessionState + Send + Sync,
{
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
            items: result.items.into_iter().map(Into::into).collect(),
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
    S: AdminSessionState + Send + Sync,
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
    S: AdminSessionState + Send + Sync,
{
    let proxy = request
        .proxy_url
        .0
        .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))?;
    let result = state
        .admin_services()
        .proxies()
        .create(
            NewProxy {
                name: request.name,
                proxy,
            },
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(MutationView::from(result)),
    ))
}

async fn remove_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RemoveAccountRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
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
    S: AdminSessionState + Send + Sync,
{
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
        AdminEnvelope::ok(MutationView::from(result)),
    ))
}

async fn delete<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<IdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
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
    S: AdminSessionState + Send + Sync,
{
    let proxy = request
        .proxy_url
        .0
        .ok_or_else(|| AdminError::bad_request("代理 URL 不能为空"))?;
    let result = state
        .admin_services()
        .proxies()
        .probe(&proxy)
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
    AdminJson(request): AdminJson<IdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .proxies()
        .test(
            &request.id,
            revision(request.revision)?,
            &auth.context().mutation_context(),
        )
        .await
        .map_err(map_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyView::from(result)),
    ))
}
