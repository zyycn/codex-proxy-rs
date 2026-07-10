//! 管理端系统版本、自更新与重启 HTTP 处理器。

use std::{convert::Infallible, time::Duration};

use axum::{
    extract::{Json, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
};
use futures::Stream;
use serde::Deserialize;

use crate::{
    api::admin::{
        response::{AdminEnvelope, AdminError, AdminResponse},
        session::AdminAuth,
    },
    api::AppState,
    update::{
        service::{self, RestartAction},
        state::operation_id,
        types::UpdateError,
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateDetailQuery {
    refresh: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateRequest {
    target_version: String,
}

pub(crate) async fn version(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(service::version_data().await),
    ))
}

pub(crate) async fn update_detail(
    _auth: AdminAuth,
    Query(query): Query<UpdateDetailQuery>,
) -> Result<impl IntoResponse, AdminError> {
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(service::update_detail(query.refresh.unwrap_or(false)).await),
    ))
}

pub(crate) async fn update_event_stream(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError> {
    let receiver = service::subscribe_update_events();
    let shutdown = state.services.process_control.subscribe_shutdown();
    let stream = futures::stream::unfold(
        (receiver, shutdown, false),
        |(mut receiver, mut shutdown, close_after_send)| async move {
            if close_after_send {
                return None;
            }
            loop {
                tokio::select! {
                    _ = shutdown.recv() => return None,
                    received = receiver.recv() => match received {
                        Ok(message) => {
                            let close_after_send = message.is_terminal();
                            let id = message.id().to_string();
                            let data = serde_json::to_string(&message)
                                .unwrap_or_else(|_| "{}".to_string());
                            let event = Event::default().event("update").id(id).data(data);
                            return Some((Ok(event), (receiver, shutdown, close_after_send)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    }
                }
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub(crate) async fn perform_update(
    _auth: AdminAuth,
    payload: Option<Json<UpdateRequest>>,
) -> Result<impl IntoResponse, AdminError> {
    let target = payload.map(|Json(payload)| payload.target_version);
    let result = service::perform_update(target)
        .await
        .map_err(update_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(result),
    ))
}

pub(crate) async fn update_status(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    let status = service::update_status().map_err(update_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(status),
    ))
}

pub(crate) async fn rollback(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    let operation_id = service::rollback().await.map_err(update_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "message": "回滚完成，请重启服务。",
            "needRestart": true,
            "operationId": operation_id,
        })),
    ))
}

pub(crate) async fn restart(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> Result<impl IntoResponse, AdminError> {
    let plan = service::restart_plan().map_err(update_error)?;
    let message = plan.message;
    let process_control = state.services.process_control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match plan.action {
            RestartAction::Exec(executable_path) => {
                process_control.request_restart(executable_path);
            }
            RestartAction::Shutdown => process_control.request_shutdown(),
        }
    });
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "message": message,
            "operationId": operation_id("restart"),
        })),
    ))
}

fn update_error(error: UpdateError) -> AdminError {
    match error {
        UpdateError::BadRequest(message) => AdminError::bad_request(message),
        UpdateError::Conflict(message) => AdminError::conflict(message),
        UpdateError::BadGateway(message) => AdminError::bad_gateway(message),
        UpdateError::Internal(message) => AdminError::internal(message),
    }
}
