use super::*;
use std::convert::Infallible;

use bytes::Bytes;
use futures::StreamExt;
use serde_json::json;

use crate::fleet::manage::AccountTestEvent;

/// `GET /api/admin/accounts/test?id=...&modelId=...`
pub async fn test_account_connection(
    State(state): State<AppState>,
    Query(query): Query<AccountTestQuery>,
    _auth: AdminAuth,
) -> Result<Response, AdminError> {
    let account_id = query.id;

    let model = query
        .model_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| AdminError::bad_request("Model is required"))?;
    let stream = state
        .services
        .admin_accounts
        .test_connection_stream(&account_id, model)
        .await
        .map_err(|error| account_error(&error))?;
    let stream = stream.map(|event| {
        Ok::<Bytes, Infallible>(Bytes::from(format!(
            "data: {}\n\n",
            account_test_event_json(event)
        )))
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .header(CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .map_err(|_| AdminError::internal("Failed to build account test stream"))
}

fn account_test_event_json(event: AccountTestEvent) -> Value {
    match event {
        AccountTestEvent::Started { model } => json!({
            "type": "test_start",
            "model": model,
            "text": "正在连接 Codex Responses"
        }),
        AccountTestEvent::Request { payload } => json!({
            "type": "request",
            "payload": payload
        }),
        AccountTestEvent::Content { text } => json!({ "type": "content", "text": text }),
        AccountTestEvent::Complete { account_status } => account_test_terminal_event(
            json!({ "type": "test_complete", "success": true }),
            account_status,
        ),
        AccountTestEvent::Error {
            error,
            account_status,
        } => {
            account_test_terminal_event(json!({ "type": "error", "error": error }), account_status)
        }
    }
}

fn account_test_terminal_event(mut event: Value, account_status: Option<AccountStatus>) -> Value {
    if let Some(account_status) = account_status
        && let Some(event) = event.as_object_mut()
    {
        event.insert(
            "accountStatus".to_string(),
            Value::String(account_status.as_str().to_string()),
        );
    }
    event
}

/// `GET /api/admin/accounts/models?id=...`
pub async fn account_models(
    State(state): State<AppState>,
    Query(query): Query<AccountIdQuery>,
    _auth: AdminAuth,
) -> Result<impl IntoResponse, AdminError> {
    let account_id = query.id;
    match state
        .services
        .admin_accounts
        .account_models(&account_id)
        .await
    {
        Ok(models) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountModelsData {
                models: models
                    .into_iter()
                    .map(|model| AccountModelData {
                        id: model.id,
                        label: model.label,
                    })
                    .collect(),
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/models`
pub async fn refresh_account_models(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let models = state
        .services
        .admin_accounts
        .refresh_account_models(&payload.id)
        .await
        .map_err(|error| account_error(&error))?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountModelsData {
            models: models
                .into_iter()
                .map(|model| AccountModelData {
                    id: model.id,
                    label: model.label,
                })
                .collect(),
        }),
    ))
}
