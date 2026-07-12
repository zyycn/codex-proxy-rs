use super::*;

/// `POST /api/admin/accounts/import`
pub async fn import_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(payload): Json<Value>,
) -> Result<impl IntoResponse, AdminError> {
    match state.services.admin_accounts.import(payload).await {
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
