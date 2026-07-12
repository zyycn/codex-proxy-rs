use super::*;

/// `GET /api/admin/accounts/export`
pub async fn export_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<AccountExportQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let ids = account_export_ids(query.ids.as_deref());

    if query.confirm.as_deref() != Some(ACCOUNT_EXPORT_CONFIRMATION) {
        return Err(AdminError::bad_request(
            "account export requires confirm=export_sensitive_accounts",
        ));
    }

    match state.services.admin_accounts.export(ids).await {
        Ok(result) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(result),
        )),
        Err(error) => Err(account_error(&error)),
    }
}
