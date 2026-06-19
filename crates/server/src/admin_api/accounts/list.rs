//! 账号列表处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_platform::json::{clamp_limit, Page};
use codex_proxy_runtime::state::AppState;
use serde::Deserialize;

use crate::{
    admin_api::{
        accounts::{account_error, AdminAccountData},
        require_admin_session, AdminError, AdminPageEnvelope, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 账号列表查询。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsQuery {
    /// 分页游标。
    pub cursor: Option<String>,
    /// 分页大小。
    pub limit: Option<u32>,
}

/// 分页列出账号元数据。
pub async fn accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let limit = clamp_limit(query.limit.unwrap_or(50));
    match state
        .services
        .admin_accounts
        .list(query.cursor, limit)
        .await
    {
        Ok(page) => {
            let page = Page {
                items: page.items.into_iter().map(AdminAccountData::from).collect(),
                next_cursor: page.next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(account_error(error, request_id)),
    }
}
