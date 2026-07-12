use super::*;

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

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .header(CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .map_err(|_| AdminError::internal("Failed to build account test stream"))
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
