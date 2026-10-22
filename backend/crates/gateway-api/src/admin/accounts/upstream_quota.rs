//! 管理端子额度的批量挂接与本地时间展示，不改变账号原生调度状态

use super::*;
use gateway_admin::model::provider_credentials::{
    ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
};
use gateway_admin::model::upstream_quota::quota_error_display;
use gateway_protocol::quota::QuotaWindowRole;

pub(super) async fn attach_upstream(
    services: &gateway_admin::AdminServices,
    accounts: &mut [AccountView],
) -> Result<(), AdminError> {
    let ids = accounts
        .iter()
        .filter(|account| account.provider == "openai" && account.authentication_kind == "api_key")
        .map(|account| {
            ProviderAccountId::new(account.id.clone()).map_err(|_| AdminError::internal())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut snapshots = services
        .upstream_quota()
        .snapshots(&ids)
        .await
        .map_err(map_service_error)?;
    let time = crate::time::TimePresenter::new(services.timezone());
    let now = Utc::now();
    for account in accounts {
        let id = ProviderAccountId::new(account.id.clone()).map_err(|_| AdminError::internal())?;
        let Some(snapshot) = snapshots.remove(&id) else {
            continue;
        };
        account.capabilities.quota = true;
        account.capabilities.quota_refresh = true;
        let entries = snapshot
            .accounts
            .into_iter()
            .map(|entry| {
                let windows = entry
                    .windows
                    .into_iter()
                    .map(|window| {
                        let remaining = window
                            .used_percent
                            .map(|used| format!("{:.1}%", 100.0 - used));
                        let mut view = quota_window_view(
                            ProviderQuotaWindow {
                                key: window.key,
                                group: window.group,
                                label: window.label,
                                limit_id: window.limit_id,
                                limit_name: window.limit_name,
                                role: window.role.map(|role| match role {
                                    QuotaWindowRole::Primary => ProviderQuotaWindowRole::Primary,
                                    QuotaWindowRole::Secondary => {
                                        ProviderQuotaWindowRole::Secondary
                                    }
                                    QuotaWindowRole::Monthly => ProviderQuotaWindowRole::Monthly,
                                }),
                                local_usage_attribution: QuotaLocalUsageAttribution::Unavailable,
                                window_seconds: window.window_seconds,
                                used_percent: window.used_percent,
                                reset_at: window.reset_at,
                                limit_reached: window.limit_reached,
                                local_usage: None,
                                provider_data: None,
                            },
                            time,
                        );
                        view.remaining_percent_display = remaining;
                        view
                    })
                    .collect();
                UpstreamQuotaAccountView {
                    id: entry.id,
                    label: entry.label,
                    models: entry.models,
                    status: entry.status,
                    observed_at: entry.observed_at.as_ref().map(|at| time.rfc3339(at)),
                    observed_at_display: time.relative_optional(entry.observed_at, now),
                    windows,
                }
            })
            .collect();
        account.quota.upstream = Some(AccountUpstreamQuotaView {
            fetched_at: snapshot.fetched_at.as_ref().map(|at| time.rfc3339(at)),
            fetched_at_display: time.relative_optional(snapshot.fetched_at, now),
            last_attempt_at: snapshot.last_attempt_at.as_ref().map(|at| time.rfc3339(at)),
            last_attempt_at_display: time.relative_optional(snapshot.last_attempt_at, now),
            last_error_display: snapshot
                .last_error_code
                .as_deref()
                .map(quota_error_display)
                .map(str::to_owned),
            last_error_code: snapshot.last_error_code,
            accounts: entries,
        });
    }
    Ok(())
}
