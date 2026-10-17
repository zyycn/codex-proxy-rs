//! 账号普通池并发明细的有界实时查询

use super::*;
use crate::auth::SessionState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Query {
    account_id: String,
    #[serde(default = "first_page")]
    page: u32,
    #[serde(default = "page_size")]
    page_size: u32,
}

const fn first_page() -> u32 {
    1
}
const fn page_size() -> u32 {
    20
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    lease_id: String,
    request_id: Option<String>,
    client_key_id: Option<String>,
    client_key_name: Option<String>,
    model: Option<String>,
    client_ip: Option<String>,
    started_at: Option<DateTime<Utc>>,
    started_at_display: Option<String>,
    occupied_ms: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    account_id: String,
    entries: Vec<Entry>,
    total: u64,
    page: u32,
    page_size: u32,
    observed_at: DateTime<Utc>,
}

pub(super) async fn list<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<Query>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    require_account_id(&query.account_id, "accountId").map_err(map_wire_error)?;
    let account_id = ProviderAccountId::new(query.account_id)
        .map_err(|_| map_wire_error(WireValidationError::new("accountId")))?;
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let result = state
        .admin_services()
        .accounts()
        .concurrency(account_id.clone(), query.page, query.page_size)
        .await
        .map_err(map_service_error)?;
    let observed_at = result.observed_at;
    let entries = result
        .entries
        .into_iter()
        .map(|entry| Entry {
            lease_id: entry.lease_id,
            request_id: entry.request_id,
            client_key_id: entry.client_key_id,
            client_key_name: entry.client_key_name,
            model: entry.model,
            client_ip: entry.client_ip,
            started_at: entry.started_at,
            started_at_display: entry.started_at.map(|at| time.datetime(&at)),
            occupied_ms: entry
                .acquired_at
                .map(|at| u64::try_from((observed_at - at).num_milliseconds()).unwrap_or(0)),
        })
        .collect();
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(Data {
            account_id: account_id.as_str().to_owned(),
            entries,
            total: result.total,
            page: query.page,
            page_size: query.page_size,
            observed_at,
        }),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
