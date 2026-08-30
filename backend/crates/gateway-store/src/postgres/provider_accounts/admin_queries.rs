//! 管理账号目录的数据库分页 read model。

use super::*;

pub(crate) struct AdminAccountPageRows {
    pub(crate) config_revision: AdminRevision,
    pub(crate) accounts: Vec<ProviderAccountSummary>,
    pub(crate) total: u64,
    pub(crate) summary: AccountSummary,
}

pub(crate) async fn load_admin_account_page(
    pool: &PgPool,
    query: &AdminAccountListQuery,
    now: DateTime<Utc>,
    active_rate_limited_ids: Vec<String>,
) -> AdminStoreResult<AdminAccountPageRows> {
    validate_group_filter(pool, query.group_filter.as_ref()).await?;

    let provider_kind = query
        .provider_kind
        .as_ref()
        .map(|provider| provider.as_str().to_owned());
    let search_prefix = query.search.as_deref().map(literal_prefix_pattern);
    let status = query.status.map(|status| status.as_str().to_owned());
    let (group_mode, group_id) = match query.group_filter.as_ref() {
        None => (0_i16, None),
        Some(AccountGroupFilter::Group(group_id)) => (1_i16, Some(group_id.as_str().to_owned())),
        Some(AccountGroupFilter::Ungrouped) => (2_i16, None),
    };
    let page_size = i64::from(query.page_size.get());
    let offset = i64::from(query.page - 1).saturating_mul(page_size);
    let order_by = admin_account_order(query.sort);
    let usage_join = query.sort.is_some_and(|sort| {
        matches!(
            sort.field,
            AdminAccountSortField::Usage | AdminAccountSortField::LastUsedAt
        )
    });
    let completed_usage = completed_usage_fact_predicate("mr");
    let usage_join = if usage_join {
        format!(
            "left join (
               select mr.provider_account_ref,
                      coalesce(sum(coalesce(
                        mr.total_tokens,
                        coalesce(mr.input_tokens, 0) + coalesce(mr.output_tokens, 0)
                      )), 0)::bigint as total_tokens,
                      max(mr.started_at) as last_used_at
                 from model_requests mr
                where mr.started_at >= $2 - make_interval(days => settings.usage_retention_days::int)
                  and mr.started_at < $2
                  and {completed_usage}
                group by mr.provider_account_ref
             ) usage on usage.provider_account_ref = a.id"
        )
    } else {
        String::new()
    };

    let statement = format!(
        "with account_statuses as (
           select a.id,
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
           select config_revision, usage_retention_days
             from runtime_settings
            where id = 1
         )
         select a.id, a.provider_kind, a.name, a.email, a.upstream_user_id,
                a.upstream_account_id, a.plan_type, a.authentication_kind,
                a.credential_revision, a.has_refresh_token, a.access_token_expires_at,
                a.next_refresh_at, a.enabled, a.concurrency_limit, a.weight,
                a.credential_state, a.quota_access_state, a.quota_evidence,
                a.quota_access_observed_at, a.quota_reset_at, a.last_error_reason,
                a.last_error_message, a.credential_observed_at, a.created_at, a.updated_at,
                filtered_total.filtered_total,
                global_summary.summary_total, global_summary.summary_normal,
                global_summary.summary_quota_exhausted, global_summary.summary_rate_limited,
                global_summary.summary_disabled, global_summary.summary_error,
                settings.config_revision
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
          order by page.page_position"
    );
    // 动态片段只来自上面的封闭排序枚举与固定 usage predicate；所有请求值仍使用 bind。
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
        .fetch_all(pool)
        .await
        .map_err(|_| admin_store_error(ENTITY, postgres_unavailable("load admin account page")))?;
    let metadata = rows.first().ok_or_else(|| {
        AdminStoreError::new(
            AdminStoreErrorKind::Unavailable,
            ENTITY,
            "admin account page metadata is unavailable",
        )
    })?;
    let config_revision = revision_from_row(metadata)?;
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
            .map_err(|_| {
                admin_store_error(
                    ENTITY,
                    postgres_unavailable("validate admin account group filter"),
                )
            })?;
    if exists {
        Ok(())
    } else {
        Err(admin_store_error(
            ENTITY,
            StoreError::NotFound {
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

fn literal_prefix_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len().saturating_add(1));
    for character in value.to_lowercase().chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
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
