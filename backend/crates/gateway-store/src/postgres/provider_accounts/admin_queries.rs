//! 管理账号目录的数据库分页 read model

use super::*;

pub(crate) struct AdminAccountPageRows {
    pub(crate) config_revision: AdminRevision,
    pub(crate) default_concurrency: u64,
    pub(crate) accounts: Vec<ProviderAccountSummary>,
    pub(crate) total: u64,
    pub(crate) summary: AccountSummary,
}

pub(crate) async fn load_admin_account_page(
    pool: &PgPool,
    budget: &crate::postgres::ObservabilityQueryBudget,
    query: &AdminAccountListQuery,
    now: DateTime<Utc>,
    active_rate_limited_ids: Vec<String>,
) -> AdminStoreResult<AdminAccountPageRows> {
    validate_group_filter(pool, query.group_filter.as_ref()).await?;

    let provider_kind = query
        .provider_kind
        .as_ref()
        .map(|provider| provider.as_str().to_owned());
    let search_prefix = query
        .search
        .as_deref()
        .map(crate::postgres::literal_prefix_pattern);
    let status = query.status.map(|status| status.as_str().to_owned());
    let (group_mode, group_id) = match query.group_filter.as_ref() {
        None => (0_i16, None),
        Some(AccountGroupFilter::Group(group_id)) => (1_i16, Some(group_id.as_str().to_owned())),
        Some(AccountGroupFilter::Ungrouped) => (2_i16, None),
    };
    let page_size = i64::from(query.page_size.get());
    let offset = i64::from(query.page - 1).saturating_mul(page_size);
    let order_by = admin_account_order(query.sort);
    let needs_usage = query.sort.is_some_and(|sort| {
        matches!(
            sort.field,
            AdminAccountSortField::Usage | AdminAccountSortField::LastUsedAt
        )
    });
    let usage_join = if needs_usage {
        "left join usage_values usage on usage.provider_account_ref = a.id"
    } else {
        ""
    };

    let statement = format!(
        "with usage_values as (select * from unnest($10::text[],$11::bigint[],$12::timestamptz[]) as usage(provider_account_ref,total_tokens,last_used_at)), account_statuses as (
           select a.id, a.provider_kind,
                  case
                    when not a.enabled then 'disabled'
                    when a.credential_state <> 'ready'
                      or a.access_token_expires_at <= $2 then 'error'
                    when a.quota_access_state = 'exhausted' then 'quota_exhausted'
                    when a.id = any($1::text[]) then 'rate_limited'
                    else 'normal'
                  end as admin_status
             from provider_accounts a
         ),
         global_summary as (
           select count(*)::bigint as summary_total,
                  count(*) filter (where admin_status = 'normal')::bigint as summary_normal,
                  count(*) filter (where admin_status = 'quota_exhausted')::bigint
                    as summary_quota_exhausted,
                  count(*) filter (where admin_status = 'rate_limited')::bigint
                    as summary_rate_limited,
                  count(*) filter (where admin_status = 'disabled')::bigint
                    as summary_disabled,
                  count(*) filter (where admin_status = 'error')::bigint as summary_error
             from account_statuses
         ),
         filtered as materialized (
           select status.id, status.admin_status
             from account_statuses status
             join provider_accounts a on a.id = status.id
            where ($3::text is null or a.provider_kind = $3)
              and ($4::text is null or
                   lower(a.id) like $4 escape '\\' or
                   lower(a.name) like $4 escape '\\' or
                   lower(coalesce(a.email, '')) like $4 escape '\\' or
                   lower(coalesce(a.upstream_account_id, '')) like $4 escape '\\' or
                   lower(coalesce(a.upstream_user_id, '')) like $4 escape '\\')
              and ($5::text is null or status.admin_status = $5)
              and ($6::smallint = 0 or
                   ($6 = 1 and exists (
                     select 1
                       from account_group_accounts membership
                      where membership.provider_account_id = a.id
                        and membership.account_group_id = $7
                   )) or
                   ($6 = 2 and not exists (
                     select 1
                       from account_group_accounts membership
                      where membership.provider_account_id = a.id
                   )))
         ),
         filtered_total as (
           select count(*)::bigint as filtered_total from filtered
         ),
         settings as (
           select config_revision, usage_retention_days, max_concurrent_per_account
             from runtime_settings
            where id = 1
         )
         select coalesce(p.enabled, true) as outbound_proxy_enabled, coalesce(p.max_websocket_connections, 0) as outbound_proxy_max_websocket_connections, p.auto_location, p.detected_location_json, p.location_country, p.location_region, p.location_city, p.location_timezone, a.outbound_proxy_url, a.id, a.provider_kind, a.name, a.notes, a.email, a.upstream_user_id,
                a.upstream_account_id, a.plan_type, a.authentication_kind,
                a.credential_revision, a.has_refresh_token, a.access_token_expires_at,
                a.next_refresh_at, a.enabled, a.concurrency_limit, a.weight, a.model_access_json,
                a.credential_state, a.quota_access_state, a.quota_evidence,
                a.quota_access_observed_at, a.quota_reset_at, a.last_error_reason,
                a.last_error_message, a.credential_observed_at, a.created_at, a.updated_at,
                filtered_total.filtered_total,
                global_summary.summary_total, global_summary.summary_normal,
                global_summary.summary_quota_exhausted, global_summary.summary_rate_limited,
                global_summary.summary_disabled, global_summary.summary_error,
                settings.config_revision, settings.max_concurrent_per_account
           from filtered_total
           cross join global_summary
           cross join settings
           left join lateral (
             select ordered.*, row_number() over ()::bigint as page_position
               from (
                 select filtered.id, filtered.admin_status
                   from filtered
                   join provider_accounts a on a.id = filtered.id
                   {usage_join}
                  order by {order_by}
                  limit $8 offset $9
               ) ordered
           ) page on true
           left join provider_accounts a on a.id = page.id
           left join outbound_proxies p on p.id = a.outbound_proxy_id
          order by page.page_position"
    );
    // 排序事实与本页总数共享同一快照，避免一次查询混入不同源版本
    let rows = budget
        .run(
            pool,
            "load admin account page",
            |mut connection| async move {
                let mut snapshot =
                    crate::postgres::observability::AnalyticsSnapshot::new_for_accounts(
                        &mut connection,
                    )
                    .await?;
                let (usage_ids, usage_tokens, usage_latest) = if needs_usage {
                    crate::postgres::observability::account_usage_sort_values(&mut snapshot, now)
                        .await?
                } else {
                    (Vec::new(), Vec::new(), Vec::new())
                };
                let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
                    .bind(active_rate_limited_ids)
                    .bind(now)
                    .bind(provider_kind)
                    .bind(search_prefix)
                    .bind(status)
                    .bind(group_mode)
                    .bind(group_id)
                    .bind(page_size)
                    .bind(offset)
                    .bind(usage_ids)
                    .bind(usage_tokens)
                    .bind(usage_latest)
                    .fetch_all(&mut *snapshot.tx)
                    .await
                    .map_err(|source| postgres_unavailable("load admin account page", source))?;
                snapshot.finish().await?;
                Ok(rows)
            },
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
    let metadata = rows.first().ok_or_else(|| {
        AdminStoreError::new(
            AdminStoreErrorKind::Unavailable,
            ENTITY,
            "admin account page metadata is unavailable",
        )
    })?;
    let config_revision = revision_from_row(metadata)?;
    let default_concurrency = unsigned_metadata(metadata, "max_concurrent_per_account")?;
    let total = unsigned_metadata(metadata, "filtered_total")?;
    let summary = AccountSummary {
        total: unsigned_metadata(metadata, "summary_total")?,
        normal: unsigned_metadata(metadata, "summary_normal")?,
        quota_exhausted: unsigned_metadata(metadata, "summary_quota_exhausted")?,
        rate_limited: unsigned_metadata(metadata, "summary_rate_limited")?,
        disabled: unsigned_metadata(metadata, "summary_disabled")?,
        error: unsigned_metadata(metadata, "summary_error")?,
    };
    let accounts = rows
        .into_iter()
        .filter_map(|row| {
            row.try_get::<Option<String>, _>("id")
                .ok()
                .flatten()
                .map(|_| row)
        })
        .map(account_summary_from_row)
        .collect::<StoreResult<Vec<_>>>()
        .map_err(|error| admin_store_error(ENTITY, error))?;
    Ok(AdminAccountPageRows {
        config_revision,
        default_concurrency,
        accounts,
        total,
        summary,
    })
}

async fn validate_group_filter(
    pool: &PgPool,
    filter: Option<&AccountGroupFilter>,
) -> AdminStoreResult<()> {
    let Some(AccountGroupFilter::Group(group_id)) = filter else {
        return Ok(());
    };
    let exists =
        sqlx::query_scalar::<_, bool>("select exists(select 1 from account_groups where id = $1)")
            .bind(group_id.as_str())
            .fetch_one(pool)
            .await
            .map_err(|source| {
                admin_store_error(
                    ENTITY,
                    postgres_unavailable("validate admin account group filter", source),
                )
            })?;
    if exists {
        Ok(())
    } else {
        Err(admin_store_error(
            ENTITY,
            StoreError::NotFound {
                source: None,
                entity: "account group",
                id: group_id.as_str().to_owned(),
            },
        ))
    }
}

fn admin_account_order(sort: Option<AdminAccountSort>) -> String {
    let Some(sort) = sort else {
        return "a.id asc".to_owned();
    };
    let expression = match sort.field {
        AdminAccountSortField::Email => "a.email",
        AdminAccountSortField::Status => {
            "case filtered.admin_status when 'normal' then 0 when 'rate_limited' then 1 when 'quota_exhausted' then 2 when 'error' then 3 else 4 end"
        }
        AdminAccountSortField::PlanType => "a.plan_type",
        AdminAccountSortField::Usage => "usage.total_tokens",
        AdminAccountSortField::LastUsedAt => "usage.last_used_at",
        AdminAccountSortField::ExpiresAt => "a.access_token_expires_at",
    };
    match sort.direction {
        AdminSortDirection::Asc => format!("{expression} asc nulls first, a.id asc"),
        AdminSortDirection::Desc => format!("{expression} desc nulls last, a.id desc"),
    }
}

fn revision_from_row(row: &sqlx::postgres::PgRow) -> AdminStoreResult<AdminRevision> {
    let value = row.try_get::<i64, _>("config_revision").map_err(|_| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "persisted config revision is invalid",
        )
    })?;
    let revision = Revision::new(u64::try_from(value).map_err(|_| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "persisted config revision is invalid",
        )
    })?)
    .map_err(|error| admin_store_error(ENTITY, error))?;
    admin_revision(revision)
}

fn unsigned_metadata(row: &sqlx::postgres::PgRow, column: &'static str) -> AdminStoreResult<u64> {
    row.try_get::<i64, _>(column)
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                format!("persisted {column} is invalid"),
            )
        })
}
