//! 互联快照与金额的管理端展示投影

use gateway_admin::model::upstream_info::{ProviderUpstreamInfo, UpstreamQuotaAmount};

use super::*;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpstreamInfoView {
    pub discovered: bool,
    pub observed_at: Option<String>,
    pub observed_at_display: String,
    pub observed_at_relative: Option<crate::time::RelativeTimeView>,
    pub stale: bool,
    pub connection_revision: String,
    pub last_error: Option<String>,
    pub max_concurrency: Option<u64>,
    pub requests_per_minute: Option<u64>,
    pub pool: Option<UpstreamPoolView>,
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
        observed_at: info.observed_at.map(|value| time.rfc3339(&value)),
        observed_at_display: info
            .observed_at
            .map_or_else(|| "—".to_owned(), |value| time.datetime(&value)),
        observed_at_relative: info.observed_at.map(|value| time.relative_view(value, now)),
        stale: info.stale,
        connection_revision: info.connection_revision,
        last_error: info.last_error,
        max_concurrency: info.max_concurrency,
        requests_per_minute: info.requests_per_minute,
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
