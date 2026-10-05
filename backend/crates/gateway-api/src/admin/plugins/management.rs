//! 插件管理页面、资源、授权回调与页面模型请求的 HTTP 边界

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{
        ConnectInfo, DefaultBodyLimit, Extension, Path, Request, State, rejection::BytesRejection,
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use base64::Engine as _;
use futures::future::BoxFuture;
use gateway_admin::model::plugins::management::{
    PluginManagementRequest, PluginManagementResponse, PluginManagementTarget,
    StartPluginManagementCallback,
};
use gateway_core::{
    error::{GatewayError, GatewayErrorKind},
    policy::ClientApiKeyId,
};
use headers::{ETag, HeaderMapExt as _, IfNoneMatch};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::middleware::headers::{decode_headers, encode_headers};
use crate::{
    admin::{
        AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse,
        wire::map_admin_service_error,
    },
    auth::SessionState,
    openai::{
        error::{gateway_error_response, protocol_error_response},
        responses::{
            ResponseAuthorization, ResponsesHttpRequest, decode_request_with_body,
            execute_prepared_responses,
        },
    },
};

const MAXIMUM_BODY_BYTES: usize = 1024 * 1024;
const MAXIMUM_MODEL_BODY_BYTES: usize = 8 * 1024 * 1024;
// HTML/SVG 即使被直接导航打开也不能获得管理端 origin；执行入口由隔离 iframe 的消息桥承载
const RESOURCE_CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' blob:; style-src 'unsafe-inline' blob:; img-src data: blob:; font-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";

pub(super) fn router<S: SessionState + Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/api/admin/plugins/extensions", get(views::<S>))
        .route("/api/admin/plugins/extensions/{instance_id}/{artifact_sha256}/{revision}/callback-tickets", post(start_callback::<S>))
        .route("/api/admin/plugins/extensions/{instance_id}/{artifact_sha256}/{revision}/api/{*path}", any(handle::<S>))
        .route("/api/admin/plugins/extensions/{instance_id}/{artifact_sha256}/{revision}/resources/{*path}", get(resource::<S>))
        .route("/plugins/resources/{instance_id}/{artifact_sha256}/{revision}/{*path}", get(public_resource::<S>))
        .route("/plugins/callbacks/{instance_id}/{artifact_sha256}/{revision}/{*path}", get(callback::<S>))
}

pub(in crate::admin) fn model_router() -> Router<crate::ApiState> {
    Router::new().route(
        "/api/admin/plugins/extensions/{instance_id}/{artifact_sha256}/{revision}/models/responses",
        post(model_responses).layer(DefaultBodyLimit::max(MAXIMUM_MODEL_BODY_BYTES)),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelResponsesQuery {
    client_key_id: String,
}

#[derive(Deserialize)]
struct TargetPath {
    instance_id: String,
    artifact_sha256: String,
    revision: u64,
}

impl TargetPath {
    fn target(&self) -> PluginManagementTarget {
        PluginManagementTarget {
            instance_id: self.instance_id.clone(),
            artifact_sha256: self.artifact_sha256.clone(),
            revision: self.revision,
        }
    }
}

async fn model_responses(
    _: AdminAuth,
    State(state): State<crate::ApiState>,
    Path(path): Path<TargetPath>,
    AdminQuery(query): AdminQuery<ModelResponsesQuery>,
    parts: axum::http::request::Parts,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, AdminError> {
    let body = body.map_err(|error| {
        AdminError::invalid_request(error.status(), "插件模型请求正文过大或无法读取")
    })?;
    let target = path.target();
    state
        .admin_services()
        .plugin_management()
        .validate_target(&target)
        .await
        .map_err(map_admin_service_error)?;
    let client_key_id = ClientApiKeyId::new(query.client_key_id)
        .map_err(|_| AdminError::bad_request("Client Key 标识不合法"))?;
    // 推理身份由显式 Key 决定；原始 HTTP 头仍完整交给插件，原生上游自行构造认证
    let headers = parts.headers;
    let (decoded, middleware_body) =
        match decode_request_with_body(&body, &headers, MAXIMUM_MODEL_BODY_BYTES) {
            Ok(decoded) => decoded,
            Err(error) => {
                return Ok(protocol_error_response(
                    StatusCode::BAD_REQUEST,
                    error.protocol_body(),
                ));
            }
        };
    let service = state.openai().clone();
    let execution = service.execution();
    let prepared = match execution.prepare_plugin_execution(&client_key_id).await {
        Ok(prepared) => prepared,
        Err(error) => return Ok(gateway_error_response(&error)),
    };
    let authorization: Arc<dyn ResponseAuthorization> =
        Arc::new(PluginModelsAuthorization { state, target });
    let response = execute_prepared_responses(
        service,
        prepared,
        ResponsesHttpRequest {
            peer_address: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0),
            ingress_id: parts
                .extensions
                .get::<tower_http::request_id::RequestId>()
                .cloned()
                .map(Extension),
            headers,
            decoded,
            middleware_body,
        },
        Some(authorization),
    )
    .await;
    Ok(response)
}

struct PluginModelsAuthorization {
    state: crate::ApiState,
    target: PluginManagementTarget,
}

impl ResponseAuthorization for PluginModelsAuthorization {
    fn authorize(&self) -> BoxFuture<'_, Result<(), GatewayError>> {
        Box::pin(async move {
            self.state
                .admin_services()
                .plugin_management()
                .validate_target(&self.target)
                .await
                .map_err(plugin_model_authorization_error)
        })
    }
}

fn plugin_model_authorization_error(error: gateway_admin::model::AdminError) -> GatewayError {
    let (kind, message) = match error.kind() {
        gateway_admin::model::AdminErrorKind::Unavailable
        | gateway_admin::model::AdminErrorKind::Internal => (
            GatewayErrorKind::ProviderInfrastructureUnavailable,
            "plugin model authorization is unavailable",
        ),
        _ => (
            GatewayErrorKind::PolicyDenied,
            "plugin model authorization was revoked",
        ),
    };
    GatewayError::new(kind, message)
}

async fn start_callback<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    Path((instance_id, artifact_sha256, revision)): Path<(String, String, u64)>,
    AdminJson(command): AdminJson<StartPluginManagementCallback>,
) -> Result<impl IntoResponse, AdminError> {
    let target = PluginManagementTarget {
        instance_id,
        artifact_sha256,
        revision,
    };
    let ticket = state
        .admin_services()
        .plugin_management()
        .start_callback(&target, command, auth.context())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ticket),
    ))
}

async fn callback<S: SessionState + Send + Sync>(
    State(state): State<S>,
    Path(path): Path<ResourcePath>,
    request: Request,
) -> Result<Response, AdminError> {
    // Axum 为 GET 路由自动提供 HEAD；预取或探测不能消耗一次性登录票据
    if request.method() != axum::http::Method::GET {
        return Err(AdminError::invalid_request(
            StatusCode::METHOD_NOT_ALLOWED,
            "插件登录回调仅允许 GET",
        ));
    }
    let query = request.uri().query().unwrap_or_default().to_owned();
    if query.len() > 8192 {
        return Err(AdminError::bad_request("插件登录回调查询过大"));
    }
    let mut values =
        url::form_urlencoded::parse(query.as_bytes()).filter(|(name, _)| name == "state");
    let nonce = values
        .next()
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AdminError::bad_request("插件登录回调缺少 state"))?;
    let headers = encode_headers(request.headers());
    if values.next().is_some() || to_bytes(request.into_body(), 0).await.is_err() {
        return Err(AdminError::bad_request("插件登录回调 state 重复或包含正文"));
    }
    let result = state
        .admin_services()
        .plugin_management()
        .callback(
            &path.target(),
            &nonce,
            PluginManagementRequest {
                method: "GET".into(),
                path: path.path,
                query,
                content_type: None,
                headers,
                body: Vec::new(),
                request_id: uuid::Uuid::now_v7().to_string(),
            },
        )
        .await
        .map_err(map_admin_service_error)?;
    raw_response(result)
}

#[derive(Deserialize)]
struct ResourcePath {
    instance_id: String,
    artifact_sha256: String,
    revision: u64,
    path: String,
}

impl ResourcePath {
    fn target(&self) -> PluginManagementTarget {
        PluginManagementTarget {
            instance_id: self.instance_id.clone(),
            artifact_sha256: self.artifact_sha256.clone(),
            revision: self.revision,
        }
    }
}

async fn views<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError> {
    let views = state
        .admin_services()
        .plugin_management()
        .views()
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(views)))
}

async fn handle<S: SessionState + Send + Sync>(
    auth: AdminAuth,
    State(state): State<S>,
    Path(path): Path<ResourcePath>,
    request: Request,
) -> Result<Response, AdminError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| AdminError::bad_request("Content-Type 无效"))?;
    let target = path.target();
    let method = request.method().as_str().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let headers = encode_headers(request.headers());
    let body = to_bytes(request.into_body(), MAXIMUM_BODY_BYTES)
        .await
        .map_err(|_| {
            AdminError::invalid_request(StatusCode::PAYLOAD_TOO_LARGE, "插件管理正文过大或无法读取")
        })?;
    let result = state
        .admin_services()
        .plugin_management()
        .handle(
            &target,
            PluginManagementRequest {
                method,
                path: path.path,
                query,
                content_type,
                headers,
                body: body.to_vec(),
                request_id: auth.context().request_id.clone(),
            },
        )
        .await
        .map_err(map_admin_service_error)?;
    raw_response(result)
}

async fn resource<S: SessionState + Send + Sync>(
    _: AdminAuth,
    State(state): State<S>,
    Path(path): Path<ResourcePath>,
    headers: HeaderMap,
) -> Result<Response, AdminError> {
    load_resource(&state, path, false, &headers).await
}

async fn public_resource<S: SessionState + Send + Sync>(
    State(state): State<S>,
    Path(path): Path<ResourcePath>,
    headers: HeaderMap,
) -> Result<Response, AdminError> {
    // 只分派已授予 public_resource 的不可变静态字节，不启动 handler 或继承 Cookie 权限
    load_resource(&state, path, true, &headers).await
}

async fn load_resource<S: SessionState + Send + Sync>(
    state: &S,
    path: ResourcePath,
    public: bool,
    headers: &HeaderMap,
) -> Result<Response, AdminError> {
    let result = state
        .admin_services()
        .plugin_management()
        .resource(&path.target(), &path.path, public)
        .await
        .map_err(map_admin_service_error)?;
    // 先复核会话、实例版本和资源授权；条件请求不能绕过撤销检查
    if result.status != 200 || result.body.len() > MAXIMUM_BODY_BYTES {
        return raw_response(result);
    }
    let mut digest = Sha256::new();
    digest.update(result.content_type.as_bytes());
    digest.update([0]);
    digest.update(&result.body);
    let etag: ETag = format!(
        "\"{}\"",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.finalize())
    )
    .parse()
    .map_err(|_| AdminError::bad_gateway())?;
    let not_modified = headers
        .typed_get::<IfNoneMatch>()
        .is_some_and(|condition| !condition.precondition_passes(&etag));
    let mut response = raw_response(result)?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    response.headers_mut().typed_insert(etag);
    if not_modified {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        *response.body_mut() = Body::empty();
    }
    Ok(response)
}

fn raw_response(result: PluginManagementResponse) -> Result<Response, AdminError> {
    if result.body.len() > MAXIMUM_BODY_BYTES || !(200..=599).contains(&result.status) {
        return Err(AdminError::bad_gateway());
    }
    let content_type =
        HeaderValue::from_str(&result.content_type).map_err(|_| AdminError::bad_gateway())?;
    let mut response = Response::new(Body::from(Bytes::from_owner(result.body)));
    *response.status_mut() =
        StatusCode::from_u16(result.status).map_err(|_| AdminError::bad_gateway())?;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(RESOURCE_CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
    // 宿主提供默认值，插件显式字段随后覆盖；保留同名多值，只验证 HTTP 语法
    headers.extend(decode_headers(result.headers).map_err(|_| AdminError::bad_gateway())?);
    Ok(response)
}
