//! 当前 Key 的单页用量 API；只暴露页面需要的只读字段

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get},
};

use crate::{
    admin::{AdminEnvelope, AdminError, AdminQuery, AdminResponse, wire::map_admin_service_error},
    auth::SessionState,
    session_cookie,
};

mod presenter;
mod query;

pub(crate) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/key-usage/version", get(version::<S>))
        .route("/api/key-usage/config", get(config::<S>))
        .route("/api/key-usage/overview", get(overview::<S>))
        .route("/api/key-usage/records", get(records::<S>))
        .route("/api/key-usage", any(not_found))
        .route("/api/key-usage/{*path}", any(not_found))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::map_response(no_store))
}

async fn version<S>(
    State(state): State<S>,
    headers: HeaderMap,
    AdminQuery(_): AdminQuery<query::EmptyQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let version = state
        .admin_services()
        .key_usage()
        .version(session_cookie::value(&headers).as_deref())
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(presenter::version(version)),
    ))
}

async fn config<S>(
    State(state): State<S>,
    headers: HeaderMap,
    AdminQuery(_): AdminQuery<query::EmptyQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let secret = state
        .admin_services()
        .key_usage()
        .config(session_cookie::value(&headers).as_deref())
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(presenter::config(secret)),
    ))
}

async fn overview<S>(
    State(state): State<S>,
    headers: HeaderMap,
    AdminQuery(query): AdminQuery<query::OverviewQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let overview = state
        .admin_services()
        .key_usage()
        .overview(
            session_cookie::value(&headers).as_deref(),
            query.into_domain(state.admin_services().timezone())?,
        )
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(presenter::overview(overview, time)),
    ))
}

async fn records<S>(
    State(state): State<S>,
    headers: HeaderMap,
    AdminQuery(query): AdminQuery<query::RecordsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let records = state
        .admin_services()
        .key_usage()
        .records(
            session_cookie::value(&headers).as_deref(),
            query.into_domain(state.admin_services().timezone())?,
        )
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(presenter::records(records, time)),
    ))
}

async fn not_found() -> AdminError {
    AdminError::not_found("密钥用量接口不存在")
}
async fn method_not_allowed() -> AdminError {
    AdminError::method_not_allowed()
}
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
