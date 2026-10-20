//! API Key 上游余额的按需读取、刷新及安全展示

use super::*;
use crate::auth::SessionState;
use gateway_admin::model::account_balance::AccountBalance;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BalanceEntry {
    label: String,
    remaining: Option<String>,
    used: Option<String>,
    total: Option<String>,
    unit: String,
    unlimited: bool,
    is_valid: bool,
    invalid_message: Option<String>,
    reset_at: Option<DateTime<Utc>>,
    reset_at_display: Option<String>,
    details: Vec<BalanceDetail>,
}

#[derive(Serialize)]
struct BalanceDetail {
    label: String,
    amount: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Balance {
    query_type: String,
    observed_at: Option<DateTime<Utc>>,
    observed_at_display: Option<String>,
    entries: Vec<BalanceEntry>,
}

#[derive(Serialize)]
struct Data {
    balance: Option<Balance>,
}

fn present(balance: AccountBalance, time: crate::time::TimePresenter) -> Balance {
    Balance {
        query_type: balance.query_type,
        observed_at: balance.observed_at,
        observed_at_display: balance.observed_at.map(|at| time.datetime(&at)),
        entries: balance
            .entries
            .into_iter()
            .map(|entry| BalanceEntry {
                label: entry.label,
                remaining: entry.remaining,
                used: entry.used,
                total: entry.total,
                unit: entry.unit,
                unlimited: entry.unlimited,
                is_valid: entry.is_valid,
                invalid_message: entry.invalid_message,
                reset_at: entry.reset_at,
                reset_at_display: entry.reset_at.map(|at| time.datetime(&at)),
                details: entry
                    .details
                    .into_iter()
                    .map(|detail| BalanceDetail {
                        label: detail.label,
                        amount: detail.amount,
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub(super) async fn read<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<AccountIdQuery>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    respond(&state, query.into_id().map_err(map_wire_error)?, false).await
}

pub(super) async fn refresh<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<AccountActionRequest>,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    respond(&state, request.into_id().map_err(map_wire_error)?, true).await
}

async fn respond<S>(
    state: &S,
    account_id: ProviderAccountId,
    refresh: bool,
) -> Result<Response, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let balance = state
        .admin_services()
        .accounts()
        .balance(&account_id, refresh)
        .await
        .map_err(map_service_error)?;
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(Data {
            balance: balance.map(|balance| present(balance, time)),
        }),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
