//! 互联快照展示与异步刷新 HTTP 合同

use gateway_admin::model::upstream_info::{ProviderUpstreamInfo, UpstreamQuotaAmount};

use super::*;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpstreamInfoView {
    pub discovered: bool,
    pub node: Option<UpstreamNodeView>,
    pub observed_at: Option<String>,
    pub observed_at_display: String,
    pub observed_at_relative: Option<crate::time::RelativeTimeView>,
    pub stale: bool,
    pub connection_revision: String,
    pub sync: UpstreamSyncView,
    pub last_error: Option<String>,
    pub pool: Option<UpstreamPoolView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamNodeView {
    pub id: String,
    pub name: String,
    pub software: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamSyncView {
    pub state: String,
    pub request_generation: u64,
    pub completed_generation: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamPoolView {
    pub account_count: u32,
    pub states: UpstreamPoolStatesView,
    pub plans: Vec<UpstreamPlanView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamPoolStatesView {
    pub ready: u32,
    pub cooldown: u32,
    pub exhausted: u32,
    pub disabled: u32,
    pub error: u32,
    pub unknown: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamPlanView {
    pub name: String,
    pub count: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshReceiptView {
    account_id: String,
    connection_revision: String,
    request_generation: u64,
    state: String,
}

#[derive(Serialize)]
struct RefreshData {
    items: Vec<RefreshReceiptView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaAmountView {
    pub used_display: String,
    pub remaining_display: String,
    pub total_display: String,
}

pub(super) fn amount_view(amount: &UpstreamQuotaAmount) -> AccountQuotaAmountView {
    let display = |value: Option<&str>| {
        value.map_or_else(
            || "∞".to_owned(),
            |value| crate::admin::presenter::format_decimal_currency(value, &amount.unit),
        )
    };
    AccountQuotaAmountView {
        used_display: display(Some(&amount.used)),
        remaining_display: display(amount.remaining.as_deref()),
        total_display: display(amount.total.as_deref()),
    }
}

pub(crate) fn view(
    info: ProviderUpstreamInfo,
    now: DateTime<Utc>,
    time: crate::time::TimePresenter,
) -> AccountUpstreamInfoView {
    AccountUpstreamInfoView {
        discovered: info.discovered,
        node: info.node.map(|node| UpstreamNodeView {
            id: node.id,
            name: node.name,
            software: node.software,
        }),
        observed_at: info.observed_at.map(|value| time.rfc3339(&value)),
        observed_at_display: info
            .observed_at
            .map_or_else(|| "—".to_owned(), |value| time.datetime(&value)),
        observed_at_relative: info.observed_at.map(|value| time.relative_view(value, now)),
        stale: info.stale,
        connection_revision: info.connection_revision,
        sync: UpstreamSyncView {
            state: info.sync.state.as_str().to_owned(),
            request_generation: info.sync.request_generation,
            completed_generation: info.sync.completed_generation,
        },
        last_error: info.last_error,
        pool: info.pool.map(|pool| UpstreamPoolView {
            account_count: pool.account_count,
            states: UpstreamPoolStatesView {
                ready: pool.states.ready,
                cooldown: pool.states.cooldown,
                exhausted: pool.states.exhausted,
                disabled: pool.states.disabled,
                error: pool.states.error,
                unknown: pool.states.unknown,
            },
            plans: pool
                .plans
                .into_iter()
                .map(|plan| UpstreamPlanView {
                    name: plan.name,
                    count: plan.count,
                })
                .collect(),
        }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RefreshRequest {
    account_ids: Vec<String>,
}

pub(super) async fn refresh<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<RefreshRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: crate::auth::SessionState + Send + Sync,
{
    if request.account_ids.is_empty() || request.account_ids.len() > 200 {
        return Err(map_wire_error(WireValidationError::new("accountIds")));
    }
    let ids = request
        .account_ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| {
            require_account_id(&id, "accountIds")?;
            ProviderAccountId::new(id).map_err(|_| WireValidationError::new("accountIds"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_wire_error)?;
    let items = state
        .admin_services()
        .accounts()
        .refresh_upstream_info(ids)
        .await
        .map_err(map_service_error)?;
    let items = items
        .into_iter()
        .map(|item| RefreshReceiptView {
            account_id: item.account_id,
            connection_revision: item.connection_revision,
            request_generation: item.request_generation,
            state: item.state.as_str().to_owned(),
        })
        .collect();
    Ok(AdminResponse::new(
        StatusCode::ACCEPTED,
        AdminEnvelope::ok(RefreshData { items }),
    ))
}
