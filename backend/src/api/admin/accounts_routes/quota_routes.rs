use super::*;

/// `GET /api/admin/accounts/quota`
pub async fn account_quota(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let account_id = query.id;
    match state
        .services
        .admin_accounts
        .account_quota(&account_id)
        .await
    {
        Ok(data) => {
            let quota = serde_json::to_value(&data.quota)
                .map_err(|error| AdminError::internal(error.to_string()))?;
            let raw = data.raw;
            let quota_read = AccountQuotaReadModel::from_snapshot(data.quota, Some(Utc::now()));
            let account = state
                .services
                .admin_accounts
                .get(&account_id)
                .await
                .map_err(|error| account_error(&error))?
                .ok_or_else(account_not_found)?;
            let item = state
                .services
                .account_list
                .enrich_account(account, quota_read)
                .await
                .map_err(|error| AdminError::internal(error.to_string()))?;
            let quota_data = item.quota.clone().map(quota_data).unwrap_or_default();
            let account = account_list_item_data(item);
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(AdminAccountQuotaResponseData::from_account(
                    quota, raw, quota_data, account,
                )),
            ))
        }
        Err(AccountManageError::NotFound) => Err(account_not_found()),
        Err(AccountManageError::Inactive(status)) => Err(AdminError::conflict(format!(
            "Account is {}, cannot query quota",
            status.as_str()
        ))),
        Err(AccountManageError::FetchQuota(msg)) => Err(AdminError::bad_gateway(format!(
            "Failed to fetch quota from Codex API: {msg}"
        ))),
        Err(e) => Err(account_error(&e)),
    }
}
