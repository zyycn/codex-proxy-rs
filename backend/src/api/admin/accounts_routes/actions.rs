use super::*;

/// `GET /api/admin/accounts/export`
pub(crate) async fn export_accounts(
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

/// `POST /api/admin/accounts/refresh`
pub(crate) async fn refresh_account(
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
pub(crate) async fn health_check_accounts(
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
    if let Some(concurrency) = payload.concurrency {
        if !(1..=10).contains(&concurrency) {
            return Err(AdminError::bad_request(
                "concurrency must be between 1 and 10",
            ));
        }
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

/// `POST /api/admin/accounts/import`
pub(crate) async fn import_accounts(
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

/// `GET /api/admin/accounts/test?id=...&modelId=...`
pub(crate) async fn test_account_connection(
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
pub(crate) async fn account_models(
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

/// `POST /api/admin/accounts/delete`
pub(crate) async fn batch_delete_accounts(
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
pub(crate) async fn update_account(
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
