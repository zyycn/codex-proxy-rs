use super::*;

/// `POST /api/admin/accounts/oauth/authorize`
pub(crate) async fn oauth_authorize_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> Result<impl IntoResponse, AdminError> {
    match state.services.admin_accounts.oauth_authorize().await {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountOAuthAuthorizeData {
                session_id: result.session_id,
                auth_url: result.auth_url,
                expires_at: china_rfc3339(&result.expires_at),
                expires_at_display: china_datetime(&result.expires_at),
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}

/// `POST /api/admin/accounts/oauth/exchange`
pub(crate) async fn oauth_exchange_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<AccountOAuthExchangeRequest>,
) -> Result<impl IntoResponse, AdminError> {
    match state
        .services
        .admin_accounts
        .oauth_exchange(OAuthExchangeInput {
            session_id: payload.session_id,
            callback_url: payload.callback_url,
            code: payload.code,
            state: payload.state,
        })
        .await
    {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(AccountImportData {
                imported: result.imported,
                skipped: result.skipped,
                source_format: result.source_format.to_string(),
            }),
        )),
        Err(error) => Err(account_error(&error)),
    }
}
