//! Client 会话可见的系统版本，不暴露管理端的部署与更新信息。

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::Serialize;

use crate::{admin::wire::map_admin_service_error, auth::SessionState};

use crate::admin::{AdminEnvelope, AdminError, AdminResponse};

#[derive(Debug, Serialize)]
struct ClientSystemVersionData {
    version: String,
}

pub(super) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new().route("/api/client/system/version", get(version::<S>))
}

async fn version<S>(
    State(state): State<S>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let session = state
        .admin_services()
        .auth()
        .session(crate::session_cookie::value(&headers).as_deref())
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    if !matches!(
        session.subject,
        gateway_admin::model::auth::SessionSubject::Key { .. }
    ) {
        return Err(AdminError::forbidden());
    }
    let version = state
        .admin_services()
        .system()
        .version()
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ClientSystemVersionData {
            version: version.version,
        }),
    ))
}
