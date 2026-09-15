//! 管理端权限校验与请求审计上下文。

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, request::Parts},
};
use gateway_admin::model::auth::{AdminPrincipal, AdminRequestContext};
use tower_http::request_id::RequestId;

use crate::{auth::SessionState, session_cookie};

use super::{AdminError, wire::map_admin_service_error};

const REQUEST_ID_HEADER: &str = "x-request-id";

/// 已通过管理员会话或部署级管理 API Key 鉴权的请求。
pub struct AdminAuth {
    context: AdminRequestContext,
}

impl AdminAuth {
    #[must_use]
    pub const fn context(&self) -> &AdminRequestContext {
        &self.context
    }
}

impl<S> FromRequestParts<S> for AdminAuth
where
    S: SessionState + Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let principal = require_admin_auth(state, &parts.headers).await?;
        let request_id = admin_request_id(parts).ok_or_else(AdminError::internal)?;
        Ok(Self {
            context: AdminRequestContext {
                principal,
                request_id,
            },
        })
    }
}

/// request-id 层按配置的 header 名注入，同时写入与名字无关的扩展；
/// 优先读扩展，使自定义 header 名不会让管理请求失去请求上下文。
/// header 回退覆盖未装配该层的调用方。
fn admin_request_id(parts: &Parts) -> Option<String> {
    parts
        .extensions
        .get::<RequestId>()
        .map(RequestId::header_value)
        .or_else(|| parts.headers.get(REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub async fn require_admin_session<S>(state: &S, headers: &HeaderMap) -> Result<String, AdminError>
where
    S: SessionState + Send + Sync,
{
    match state
        .admin_services()
        .auth()
        .resolve_admin_user_id(session_cookie::value(headers).as_deref())
        .await
    {
        Ok(Some(admin_user_id)) => Ok(admin_user_id),
        Ok(None) => Err(AdminError::session_required()),
        Err(error) => Err(map_admin_service_error(error)),
    }
}

async fn require_admin_auth<S>(state: &S, headers: &HeaderMap) -> Result<AdminPrincipal, AdminError>
where
    S: SessionState + Send + Sync,
{
    if let Some(api_key) = admin_api_key_header(headers) {
        return match state
            .admin_services()
            .auth()
            .verify_admin_api_key(&api_key)
            .await
        {
            Ok(true) => Ok(AdminPrincipal::ApiKey),
            Ok(false) => Err(AdminError::invalid_admin_api_key()),
            Err(error) => Err(map_admin_service_error(error)),
        };
    }

    require_admin_session(state, headers)
        .await
        .map(|admin_user_id| AdminPrincipal::Session { admin_user_id })
}

fn admin_api_key_header(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("x-api-key")?.to_str().ok()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
