use super::*;

/// `POST /api/admin/accounts/refresh`
pub async fn refresh_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let account_id = payload.id;
    match state
        .services
        .admin_accounts
        .refresh_account(&account_id)
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountRefreshData::from(result)),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/health-check`
pub async fn health_check_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<AccountHealthCheckRequest>,
) -> Result<impl IntoResponse, AdminError> {
    let stagger_ms = payload.stagger_ms.unwrap_or(3_000);
    if !(500..=30_000).contains(&stagger_ms) && stagger_ms != 0 {
        return Err(AdminError::bad_request(
            "stagger_ms must be between 500 and 30000",
        ));
    }
    if let Some(concurrency) = payload.concurrency
        && !(1..=10).contains(&concurrency)
    {
        return Err(AdminError::bad_request(
            "concurrency must be between 1 and 10",
        ));
    }
    match state
        .services
        .admin_accounts
        .health_check_accounts(payload.ids, stagger_ms, payload.concurrency.unwrap_or(2))
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountHealthCheckData::from(result)),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/delete`
pub async fn batch_delete_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<BatchDeleteAccountsRequest>,
) -> Result<impl IntoResponse, AdminError> {
    match state
        .services
        .admin_accounts
        .batch_delete(payload.ids)
        .await
    {
        Ok(result) => {
            for account_id in &result.deleted_ids {
                state
                    .services
                    .session_affinity
                    .forget_account(account_id)
                    .await;
            }
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(BatchDeleteData {
                    deleted: result.deleted,
                    not_found: result.not_found,
                }),
            ))
        }
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/update`
pub async fn update_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    let ParsedAccountUpdate { id, update } = parse_account_update(&payload)?;

    match state
        .services
        .admin_accounts
        .update_account(&id, update)
        .await
    {
        Ok(Some(account)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountUpdateData::Account(Box::new(account.into()))),
        )),
        Ok(None) => Err(account_not_found()),
        Err(error) => Err(account_error(&error)),
    }
}
