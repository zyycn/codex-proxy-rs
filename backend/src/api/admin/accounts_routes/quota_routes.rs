use super::*;

/// `GET /api/admin/accounts/quota`
pub(crate) async fn account_quota(
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
            let quota = data.get("quota").cloned().unwrap_or(Value::Null);
            let raw = data.get("raw").cloned().unwrap_or(Value::Null);
            let quota_json = quota.to_string();
            let mut quota_data = quota_data(&quota_json, Some(Utc::now()));
            apply_account_quota_window_local_usage(&state, &account_id, &mut quota_data).await;
            let account =
                account_data_for_quota_refresh(&state, &account_id, quota_data.clone()).await?;
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

async fn account_data_for_quota_refresh(
    state: &AppState,
    account_id: &str,
    quota_data: AccountQuotaData,
) -> Result<AdminAccountData, AdminError> {
    let Some(account) = state
        .services
        .admin_accounts
        .get(account_id)
        .await
        .map_err(|error| account_error(&error))?
    else {
        return Err(account_not_found());
    };
    let quota_by_account = HashMap::from([(account_id.to_string(), quota_data)]);
    let stats =
        account_list_stats(state, std::slice::from_ref(&account), &quota_by_account).await?;
    Ok(stats.data_for(account))
}
