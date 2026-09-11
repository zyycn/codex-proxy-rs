//! Outbound proxy pool HTTP wire and fixed routes.

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_admin::model::{
    PageSize,
    proxies::{
        CreateOutboundProxy, DeleteOutboundProxy, OutboundProxyId, OutboundProxyListQuery,
        OutboundProxyMutation, OutboundProxyPage, OutboundProxyRecord, OutboundProxyTestReport,
        OutboundProxyTestSubject, TestOutboundProxy, UpdateOutboundProxy,
    },
};
use gateway_core::account::OutboundProxy;
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse, AdminSessionState,
    PageMeta, WireValidationError, wire::map_admin_service_error,
};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_TEST_TARGET_BYTES: usize = 2048;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListProxiesQuery {
    page: Option<u32>,
    page_size: Option<u32>,
    search: Option<String>,
}

impl ListProxiesQuery {
    fn into_command(self) -> Result<OutboundProxyListQuery, WireValidationError> {
        let page = self.page.unwrap_or(1);
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page == 0 || page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("page"));
        }
        let search = self
            .search
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if search
            .as_deref()
            .is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(WireValidationError::new("search"));
        }
        Ok(OutboundProxyListQuery {
            page,
            page_size: PageSize::new(
                u16::try_from(page_size).map_err(|_| WireValidationError::new("pageSize"))?,
            )
            .map_err(|_| WireValidationError::new("pageSize"))?,
            search,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateProxyRequest {
    name: String,
    url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateProxyRequest {
    id: String,
    name: String,
    /// 缺省或空串保留当前 URL；非空时校验并覆盖。
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProxyIdRequest {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestProxyRequest {
    /// 池内代理 ID；与 `url` 二选一。
    #[serde(default)]
    id: Option<String>,
    /// 未保存的临时代理 URL；与 `id` 二选一。
    #[serde(default)]
    url: Option<String>,
    /// 测试目标链接；缺省使用服务端默认目标。
    #[serde(default)]
    target_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyView {
    id: String,
    name: String,
    /// 已脱敏 endpoint；不含用户名与密码。
    endpoint: String,
    account_count: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<OutboundProxyRecord> for ProxyView {
    fn from(record: OutboundProxyRecord) -> Self {
        Self {
            id: record.id.as_str().to_owned(),
            name: record.name,
            endpoint: record.proxy.endpoint(),
            account_count: record.account_count,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyPageData {
    items: Vec<ProxyView>,
    page: PageMeta,
    config_revision: u64,
}

impl From<OutboundProxyPage> for ProxyPageData {
    fn from(page: OutboundProxyPage) -> Self {
        let total_pages = if page.total == 0 {
            0
        } else {
            page.total.div_ceil(u64::from(page.page_size))
        };
        Self {
            items: page.items.into_iter().map(Into::into).collect(),
            page: PageMeta::new(
                page.page,
                u32::from(page.page_size),
                page.total,
                u32::try_from(total_pages).unwrap_or(u32::MAX),
            ),
            config_revision: page.config_revision.get(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyMutationData {
    id: String,
    record: Option<ProxyView>,
    config_revision: u64,
}

impl From<OutboundProxyMutation> for ProxyMutationData {
    fn from(mutation: OutboundProxyMutation) -> Self {
        Self {
            id: mutation.id.as_str().to_owned(),
            record: mutation.record.map(Into::into),
            config_revision: mutation.config_revision.get(),
        }
    }
}

/// 仅由显式 reveal 返回一次的完整代理 URL。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevealedProxyData {
    id: String,
    url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestData {
    success: bool,
    latency_ms: u64,
    status_code: Option<u16>,
    target_url: String,
    error: Option<String>,
}

impl From<OutboundProxyTestReport> for ProxyTestData {
    fn from(report: OutboundProxyTestReport) -> Self {
        Self {
            success: report.success,
            latency_ms: report.latency_ms,
            status_code: report.status_code,
            target_url: report.target_url,
            error: report.error,
        }
    }
}

/// Construct all fixed outbound proxy management routes.
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/proxies", get(list::<S>))
        .route("/api/admin/proxies/reveal", get(reveal::<S>))
        .route("/api/admin/proxies/create", post(create::<S>))
        .route("/api/admin/proxies/update", post(update::<S>))
        .route("/api/admin/proxies/delete", post(delete::<S>))
        .route("/api/admin/proxies/test", post(test::<S>))
}

async fn list<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ListProxiesQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .outbound_proxies()
        .list(query.into_command().map_err(map_wire_error)?)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyPageData::from(result)),
    ))
}

async fn reveal<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ProxyIdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .outbound_proxies()
        .reveal(&proxy_id(query.id)?)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RevealedProxyData {
            id: result.id.as_str().to_owned(),
            url: result.proxy.expose_url().to_owned(),
        }),
    ))
}

async fn create<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<CreateProxyRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    validate_proxy_name(&request.name)?;
    mutation_response(
        StatusCode::CREATED,
        state
            .admin_services()
            .outbound_proxies()
            .create(
                &auth.context().mutation_context(),
                CreateOutboundProxy {
                    name: request.name,
                    proxy: proxy_url(&request.url)?,
                },
            )
            .await,
    )
}

async fn update<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdateProxyRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    validate_proxy_name(&request.name)?;
    let proxy = request
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(proxy_url)
        .transpose()?;
    mutation_response(
        StatusCode::OK,
        state
            .admin_services()
            .outbound_proxies()
            .update(
                &auth.context().mutation_context(),
                UpdateOutboundProxy {
                    id: proxy_id(request.id)?,
                    name: request.name,
                    proxy,
                },
            )
            .await,
    )
}

async fn delete<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<ProxyIdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    mutation_response(
        StatusCode::OK,
        state
            .admin_services()
            .outbound_proxies()
            .delete(
                &auth.context().mutation_context(),
                DeleteOutboundProxy {
                    id: proxy_id(request.id)?,
                },
            )
            .await,
    )
}

async fn test<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<TestProxyRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let subject = match (request.id, request.url) {
        (Some(id), None) => OutboundProxyTestSubject::Saved(proxy_id(id)?),
        (None, Some(url)) => OutboundProxyTestSubject::Inline(proxy_url(&url)?),
        _ => return Err(AdminError::bad_request("代理测试请求必须指定 ID 或 URL")),
    };
    let target_url = request
        .target_url
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(target) = target_url.as_deref() {
        validate_test_target(target)?;
    }
    let result = state
        .admin_services()
        .outbound_proxies()
        .test(TestOutboundProxy {
            subject,
            target_url,
        })
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ProxyTestData::from(result)),
    ))
}

fn mutation_response(
    status: StatusCode,
    result: Result<OutboundProxyMutation, gateway_admin::model::AdminError>,
) -> Result<AdminResponse<AdminEnvelope<ProxyMutationData>>, AdminError> {
    let result = result.map_err(map_service_error)?;
    Ok(AdminResponse::new(
        status,
        AdminEnvelope::ok(ProxyMutationData::from(result)),
    ))
}

fn proxy_id(value: String) -> Result<OutboundProxyId, AdminError> {
    OutboundProxyId::new(value).map_err(|_| AdminError::bad_request("出站代理 ID 不合法"))
}

fn proxy_url(value: &str) -> Result<OutboundProxy, AdminError> {
    OutboundProxy::parse(value.trim()).map_err(|_| {
        AdminError::bad_request(
            "代理 URL 不合法；仅支持 http/https/socks5/socks5h 且需含主机和端口",
        )
    })
}

fn validate_proxy_name(name: &str) -> Result<(), AdminError> {
    if name.trim() != name
        || name.is_empty()
        || name.chars().count() > 100
        || name.chars().any(char::is_control)
    {
        return Err(AdminError::bad_request("出站代理名称不合法"));
    }
    Ok(())
}

fn validate_test_target(target: &str) -> Result<(), AdminError> {
    let invalid = || AdminError::bad_request("测试链接不合法；仅支持 http/https URL");
    if target.len() > MAX_TEST_TARGET_BYTES || target.chars().any(char::is_control) {
        return Err(invalid());
    }
    let url = url::Url::parse(target).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(invalid());
    }
    Ok(())
}

fn map_wire_error(_: WireValidationError) -> AdminError {
    AdminError::bad_request("出站代理查询参数不合法")
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error)
}
