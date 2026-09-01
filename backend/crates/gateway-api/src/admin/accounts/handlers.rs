//! 账号管理路由与 HTTP handler 编排。

use super::*;

/// 构造统一账号管理路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/accounts", get(list_accounts::<S>))
        .route("/api/admin/accounts/detail", get(account_detail::<S>))
        .route("/api/admin/accounts/export", get(export_accounts::<S>))
        .route("/api/admin/accounts/import", post(import_accounts::<S>))
        .route("/api/admin/accounts/refresh", post(refresh_account::<S>))
        .route("/api/admin/accounts/recover", post(recover_account::<S>))
        .route("/api/admin/accounts/rotate", post(rotate_account::<S>))
        .route("/api/admin/accounts/update", post(update_account::<S>))
        .route("/api/admin/accounts/delete", post(delete_accounts::<S>))
        .route(
            "/api/admin/accounts/batch-update",
            post(batch_update_accounts::<S>),
        )
        .route("/api/admin/accounts/quota", get(account_quota::<S>))
        .route(
            "/api/admin/accounts/profile-statistics",
            get(account_profile_statistics::<S>),
        )
        .route(
            "/api/admin/accounts/profile-avatar",
            get(account_profile_avatar::<S>),
        )
        .route(
            "/api/admin/accounts/reset-credits",
            get(account_reset_credits::<S>).post(consume_account_reset_credit::<S>),
        )
        .route(
            "/api/admin/accounts/quota/refresh",
            post(refresh_account_quota::<S>),
        )
        .route("/api/admin/accounts/models", get(account_models::<S>))
        .route(
            "/api/admin/accounts/models/refresh",
            post(refresh_account_models::<S>),
        )
        .route(
            "/api/admin/accounts/connection-test",
            get(test_account_connection::<S>),
        )
        .route(
            "/api/admin/accounts/oauth/start",
            post(start_account_authorization::<S>),
        )
        .route(
            "/api/admin/accounts/oauth/complete",
            post(complete_account_authorization::<S>),
        )
}

async fn batch_update_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<BatchUpdateAccountsRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .batch_update(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BatchUpdatedAccountsData::from(result)),
    ))
}

async fn list_accounts<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ListQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = query.validate().map_err(map_wire_error)?;
    let page = command.page;
    let page_size = command.page_size.get();
    let result = state
        .admin_services()
        .accounts()
        .list(command)
        .await
        .map_err(map_service_error)?;
    let data = account_page_data(result, page, page_size, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_detail<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let data = AccountQuotaData {
        account: account_view(result, Utc::now()),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn export_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountExportQuery>,
) -> Result<Response, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let ids = query.into_ids().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .export(&auth.context().mutation_context(), ids)
        .await
        .map_err(map_service_error)?;
    let data = AccountExportData::from_result(result);
    let mut response = AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn import_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountImportRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => {
            state
                .admin_services()
                .openai()
                .import_document(command)
                .await
        }
        AccountProvider::Xai => state.admin_services().xai().import_document(command).await,
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(AccountImportData::from_result(result)),
    ))
}

async fn start_account_authorization<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<StartAccountAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => {
            state
                .admin_services()
                .openai()
                .start_authorization(command)
                .await
        }
        AccountProvider::Xai => {
            state
                .admin_services()
                .xai()
                .start_authorization(command)
                .await
        }
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(AccountAuthorizationData::from(result)),
    ))
}

async fn complete_account_authorization<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<CompleteAccountAuthorizationRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => {
            state
                .admin_services()
                .openai()
                .complete_authorization(command)
                .await
        }
        AccountProvider::Xai => {
            state
                .admin_services()
                .xai()
                .complete_authorization(command)
                .await
        }
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::CREATED,
        AdminEnvelope::ok(AccountMutationData::from(result)),
    ))
}

async fn rotate_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RotateAccountRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .openai()
        .rotate(command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountMutationData::from(result)),
    ))
}

async fn update_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<UpdateAccountRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .update(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdatedAccountData::from(result)),
    ))
}

async fn delete_accounts<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountDeletionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (provider, command) = request
        .into_command(auth.context().mutation_context())
        .map_err(map_wire_error)?;
    let result = match provider {
        AccountProvider::OpenAi => state.admin_services().openai().delete(command).await,
        AccountProvider::Xai => state.admin_services().xai().delete(command).await,
    }
    .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountDeletionData::from(result)),
    ))
}

async fn refresh_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountRefreshRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .refresh(&auth.context().mutation_context(), account_id)
        .await
        .map_err(map_service_error)?;
    let data = account_refresh_data(result, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn recover_account<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .recover(&auth.context().mutation_context(), account_id)
        .await
        .map_err(map_service_error)?;
    let data = account_refresh_data(result, Utc::now());
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let data = AccountQuotaData {
        account: account_view(result, Utc::now()),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_profile_statistics<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .profile_statistics(&account_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountProfileStatisticsData::from(result)),
    ))
}

async fn account_profile_avatar<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountProfileAvatarQuery>,
) -> Result<Response, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let avatar = state
        .admin_services()
        .accounts()
        .profile_avatar(&account_id)
        .await
        .map_err(map_service_error)?;
    Ok(profile_avatar_response(avatar))
}

/// 将 Provider 头像流投影为受保护的同源 HTTP 响应。
#[must_use]
pub fn profile_avatar_response(avatar: ProviderProfileAvatar) -> Response {
    let ProviderProfileAvatar {
        content_type,
        content_length,
        etag,
        body,
    } = avatar;
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        content_type
            .and_then(|value| HeaderValue::from_str(&value).ok())
            .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream")),
    );
    if let Some(value) =
        content_length.and_then(|length| HeaderValue::from_str(&length.to_string()).ok())
    {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    if let Some(value) = etag.and_then(|value| HeaderValue::from_str(&value).ok()) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    response
}

async fn refresh_account_quota<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .quota(&account_id, true)
        .await
        .map_err(map_service_error)?;
    let data = AccountQuotaData {
        account: account_view(result, Utc::now()),
    };
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn account_reset_credits<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .reset_credits(&auth.context().mutation_context(), account_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountResetCreditsData::from(result)),
    ))
}

async fn consume_account_reset_credit<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountResetCreditConsumeRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = request.into_command().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .consume_reset_credit(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AccountResetCreditResultData::from(result)),
    ))
}

async fn account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = query.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .models(&account_id, false)
        .await
        .map_err(map_service_error)?;
    let data = account_models_data(result);
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn refresh_account_models<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let account_id = request.into_id().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .accounts()
        .models(&account_id, true)
        .await
        .map_err(map_service_error)?;
    let data = account_models_data(result);
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn test_account_connection<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountTestQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (account_id, upstream_model) = query.into_command().map_err(map_wire_error)?;
    let stream = state
        .admin_services()
        .accounts()
        .test_connection(account_id, upstream_model)
        .await
        .map_err(map_service_error)?
        .map(|event| {
            let event = AccountConnectionTestEvent::from(event);
            let data = serde_json::to_string(&event.data).unwrap_or_else(|_| "{}".to_owned());
            Ok(Event::default().data(data))
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
