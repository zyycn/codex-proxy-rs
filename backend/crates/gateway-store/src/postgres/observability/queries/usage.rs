//! Usage 明细、诊断与过滤查询族

use super::super::*;
use serde_json::Value;

const MODEL_DIAGNOSTIC_DIMENSION_SQL: &str =
    "coalesce(mr.upstream_model_id, mr.requested_model_id)";

pub(crate) fn push_usage_filter(
    query: &mut QueryBuilder<Postgres>,
    filter: &UsageRecordFilter,
    alias: &str,
) {
    for (column, value) in [
        ("client_api_key_name_snapshot", &filter.client_api_key_name),
        (
            "provider_account_name_snapshot",
            &filter.provider_account_name,
        ),
    ] {
        if let Some(value) = value {
            query.push(format!(" and {alias}.{column} like "));
            query.push_bind(literal_contains_pattern(value));
            query.push(" escape '\\'");
        }
    }
    if let Some(value) = &filter.client_api_key_ref {
        query.push(format!(" and {alias}.client_api_key_ref = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.request_id {
        query.push(format!(" and {alias}.id = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.provider_account_ref {
        query.push(format!(" and {alias}.provider_account_ref = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.operation {
        query.push(format!(" and {alias}.operation = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.provider_kind {
        query.push(format!(" and {alias}.provider_kind = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.model {
        query.push(format!(" and ({alias}.requested_model_id = "));
        query.push_bind(value.clone());
        query.push(format!(" or {alias}.upstream_model_id = "));
        query.push_bind(value.clone());
        query.push(")");
    }
    if let Some(value) = &filter.model_contains {
        let pattern = literal_contains_pattern(value);
        query.push(format!(" and ({alias}.requested_model_id like "));
        query.push_bind(pattern.clone());
        query.push(format!(" escape '\\' or {alias}.upstream_model_id like "));
        query.push_bind(pattern);
        query.push(" escape '\\')");
    }
    if let Some(value) = &filter.outcome {
        query.push(format!(" and {alias}.outcome = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = filter.status_code {
        query.push(format!(" and ({alias}.client_status_code = "));
        query.push_bind(i32::from(value));
        query.push(format!(" or {alias}.upstream_status_code = "));
        query.push_bind(i32::from(value));
        query.push(")");
    }
    if let Some(value) = &filter.transport {
        query.push(format!(" and ({alias}.client_transport = "));
        query.push_bind(value.clone());
        query.push(format!(" or {alias}.upstream_transport = "));
        query.push_bind(value.clone());
        query.push(")");
    }
    if let Some(value) = filter.attempt_index {
        query.push(format!(" and ({alias}.attempt_count = "));
        query.push_bind(i32::try_from(value).unwrap_or(i32::MAX));
        query.push(format!(
            " or exists (select 1 from ops_events attempt_event
                          where attempt_event.model_request_id = {alias}.id
                            and attempt_event.attempt_index = "
        ));
        query.push_bind(i32::try_from(value).unwrap_or(i32::MAX));
        query.push("))");
    }
    if let Some(value) = &filter.response_id {
        query.push(format!(" and ({alias}.client_response_id = "));
        query.push_bind(value.as_bytes().to_vec());
        query.push(format!(" or {alias}.upstream_response_id = "));
        query.push_bind(value.as_bytes().to_vec());
        query.push(")");
    }
    if let Some(value) = &filter.upstream_request_id {
        query.push(format!(" and {alias}.upstream_request_id = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.search {
        let pattern = literal_contains_pattern(value);
        query.push(format!(" and ({alias}.requested_model_id like "));
        query.push_bind(pattern.clone());
        for column in [
            "upstream_model_id",
            "client_api_key_ref",
            "provider_account_ref",
            "provider_account_email_snapshot",
            "provider_account_name_snapshot",
        ] {
            query.push(format!(" escape '\\' or {alias}.{column} like "));
            query.push_bind(pattern.clone());
        }
        query.push(" escape '\\'");

        query
            .push(format!(" or {alias}.client_api_key_name_snapshot ilike "))
            .push_bind(pattern)
            .push(" escape '\\'");
        query.push(")");
    }
}

pub(crate) const USAGE_LIST_RECORD_SELECT: &str =
    "select mr.id, mr.outcome, mr.provider_error_code as error_code, mr.error_message, mr.client_status_code, mr.client_api_key_name_snapshot as client_api_key_name, mr.endpoint, mr.client_transport, mr.requested_model_id,
            mr.error_kind, mr.upstream_status_code,
            case when pg_input_is_valid(mr.error_details, 'jsonb') then
              case when jsonb_typeof(mr.error_details::jsonb -> 'upstream') = 'string'
                and pg_input_is_valid(mr.error_details::jsonb ->> 'upstream', 'jsonb') then
                (select case when jsonb_array_length(codes) = 1 then codes ->> 0 end
                 from jsonb_path_query_array((mr.error_details::jsonb ->> 'upstream')::jsonb,
                   '$ ? (@.type == \"websocket.close\").code ? (@.type() == \"number\")') as codes)
              end
            end as websocket_close_code,
            mr.continuation_unavailable_reason, mr.upstream_connection_exit_reason,
            (mr.recovery_request_id is not null and mr.recovered_at is not null) as recovered,
            mr.provider_kind, mr.provider_account_ref,
            mr.provider_account_name_snapshot as provider_account_name,
            mr.provider_account_email_snapshot as provider_account_email,
            mr.provider_account_notes_snapshot as provider_account_notes,
            mr.provider_account_plan_type_snapshot as provider_account_plan_type,
            mr.provider_account_authentication_kind_snapshot
              as provider_account_authentication_kind,
            mr.upstream_model_id, mr.upstream_transport, mr.service_tier,
            mr.input_tokens, mr.output_tokens, mr.cached_input_tokens, mr.cache_write_input_tokens,
            mr.reasoning_output_tokens, mr.image_input_tokens, mr.image_output_tokens,
            mr.total_tokens, mr.billing_snapshot_json, mr.cost_source, mr.cost_amount::text, mr.cost_currency,
            mr.transport_decision_wait_ms, mr.connect_ms, mr.headers_ms,
            mr.first_event_ms, mr.first_reasoning_ms, mr.first_text_ms, mr.first_token_ms,
            mr.provider_processing_ms, mr.upstream_response_ms,
            mr.upstream_api_overhead_ms, mr.upstream_engine_ms, mr.latency_ms, mr.admission_decision_ms,
            mr.account_selection_wait_ms, mr.capacity_used_slots, mr.capacity_total_slots,
            host(mr.client_ip) as client_ip, mr.user_agent,
            mr.reasoning_effort, mr.reasoning_preset, mr.subagent_kind, mr.compact,
            mr.started_at
     from model_request_observations mr";

pub(crate) const USAGE_RECORD_DETAIL_SELECT: &str =
    "select mr.id, mr.client_api_key_ref, mr.config_revision,
            mr.routing_scope, mr.routing_group_refs, mr.routing_group_names_snapshot,
            mr.protocol, mr.operation,
            mr.endpoint, mr.client_transport, mr.requested_model_id,
            mr.provider_kind, mr.provider_account_ref,
            mr.provider_account_name_snapshot as provider_account_name,
            mr.provider_account_email_snapshot as provider_account_email,
            mr.provider_account_authentication_kind_snapshot
              as provider_account_authentication_kind,
            mr.upstream_model_id, mr.upstream_transport, mr.http_version, mr.websocket_pool,
            mr.service_tier, mr.provider_observation_json, mr.diagnostic_trace_json,
            mr.attempt_count, mr.upstream_send_state, mr.downstream_committed_at,
            mr.outcome, mr.client_status_code, mr.upstream_status_code,
            mr.client_response_id, mr.upstream_request_id, mr.upstream_response_id,
            mr.error_kind, mr.provider_error_code, mr.error_message, mr.retry_after_ms,
            mr.error_details, mr.continuation_unavailable_reason, mr.upstream_connection_exit_reason,
            (mr.recovery_request_id is not null and mr.recovered_at is not null) as recovered,
            mr.input_tokens, mr.output_tokens, mr.cached_input_tokens, mr.cache_write_input_tokens,
            mr.reasoning_output_tokens, mr.image_input_tokens, mr.image_output_tokens,
            mr.total_tokens, mr.billing_snapshot_json, mr.cost_source, mr.cost_amount::text,
            mr.cost_currency, mr.transport_decision_wait_ms, mr.connect_ms, mr.headers_ms,
            mr.first_event_ms, mr.first_reasoning_ms, mr.first_text_ms, mr.first_token_ms,
            mr.provider_processing_ms, mr.upstream_response_ms,
            mr.upstream_api_overhead_ms, mr.upstream_engine_ms, mr.latency_ms, mr.admission_decision_ms,
            mr.account_selection_wait_ms, mr.capacity_used_slots, mr.capacity_total_slots,
            host(mr.client_ip) as client_ip,
            mr.user_agent, mr.reasoning_effort, mr.reasoning_preset, mr.request_kind,
            mr.subagent_kind, mr.compact, mr.image_generation_requested,
            mr.image_generation_succeeded, mr.started_at, mr.effective_recovery_lease_expires_at as deadline_at, mr.completed_at
     from model_request_observations mr";

pub(crate) async fn list_usage_records(
    connection: &mut ObservabilityConnection,
    query: UsageRecordQuery,
) -> StoreResult<UsageRecordPage> {
    query.filter.validate()?;
    let mut snapshot = AnalyticsSnapshot::new_for_accounts(connection).await?;
    // 第一页未填满时，当前快照中的行数已是精确总数，无需再次扫描计数
    let (total, items) = if query.current_page == 1 {
        let items = list_usage_record_items(&mut *snapshot.tx, &query, None).await?;
        let total = if items.len() < usize::from(query.page_size.get()) {
            items.len() as u64
        } else if let Some(counts) = projected_usage_counts(&mut snapshot, &query).await? {
            counts.values().try_fold(0_u64, |sum, count| {
                sum.checked_add(*count)
                    .ok_or_else(|| invalid("usage count overflows u64"))
            })?
        } else {
            count_usage_records(&mut *snapshot.tx, query.range, &query.filter, query.status).await?
        };
        (total, items)
    } else {
        let (total, page) =
            if let Some(counts) = projected_usage_counts(&mut snapshot, &query).await? {
                time_page_from_segments(
                    counts.into_iter().rev(),
                    query.range,
                    observability_page_offset(query.current_page, query.page_size)?,
                    query.page_size,
                    TimeDelta::minutes(15),
                )?
            } else {
                usage_time_page(&mut snapshot.tx, &query).await?
            };
        let items = match page {
            Some(page) => list_usage_record_items_at(&mut *snapshot.tx, &query, page).await?,
            None => Vec::new(),
        };
        (total, items)
    };
    snapshot.finish().await?;
    Ok(UsageRecordPage {
        items,
        current_page: query.current_page,
        page_size: query.page_size.get(),
        total,
    })
}

async fn projected_usage_counts(
    snapshot: &mut AnalyticsSnapshot<'_>,
    query: &UsageRecordQuery,
) -> StoreResult<Option<BTreeMap<DateTime<Utc>, u64>>> {
    use admin_observability::UsageRecordStatus;
    if query.status == UsageRecordStatus::Error {
        return Ok(None);
    }
    let Some(mut counts) = snapshot
        .completed_request_counts(query.range, &query.filter)
        .await?
    else {
        return Ok(None);
    };
    if query.status == UsageRecordStatus::All {
        // 成功计数已校正未投影差量；其他状态直接读同一快照源表，保留恢复记录和执行中请求
        let mut statement = QueryBuilder::<Postgres>::new(
            "select date_bin(interval '15 minutes',mr.started_at,timestamptz 'epoch') as bucket_start,
                    count(*)::bigint as request_count
             from model_request_observations mr where mr.started_at >= ",
        );
        statement.push_bind(query.range.start);
        statement.push(" and mr.started_at < ");
        statement.push_bind(query.range.end);
        statement.push(" and mr.outcome in ('running','failed','cancelled','incomplete')");
        push_non_success_count_filter(&mut statement, &query.filter);
        statement.push(" group by bucket_start");
        let rows = statement
            .build()
            .fetch_all(&mut *snapshot.tx)
            .await
            .map_err(|source| postgres_unavailable("count non-success usage records", source))?;
        for row in rows {
            let count = counts.entry(get(&row, "bucket_start")?).or_default();
            *count = count
                .checked_add(to_u64(get(&row, "request_count")?)?)
                .ok_or_else(|| invalid("usage count overflows u64"))?;
        }
    }
    Ok(Some(counts))
}

pub(crate) async fn list_usage_record_items<'e, E: sqlx::Executor<'e, Database = Postgres>>(
    executor: E,
    query: &UsageRecordQuery,
    total: Option<u64>,
) -> StoreResult<Vec<UsageListRecord>> {
    let page = TimePage {
        range: query.range,
        offset: observability_page_offset(query.current_page, query.page_size)?,
        total,
    };
    list_usage_record_items_at(executor, query, page).await
}

async fn list_usage_record_items_at<'e, E: sqlx::Executor<'e, Database = Postgres>>(
    executor: E,
    query: &UsageRecordQuery,
    page: TimePage,
) -> StoreResult<Vec<UsageListRecord>> {
    query.filter.validate()?;
    let mut offset = page.offset;
    let mut limit = i64::from(query.page_size.get());
    let mut ascending = false;
    if let Some(total) = page.total {
        let total = i64::try_from(total).map_err(|_| invalid("usage count overflows i64"))?;
        if offset >= total {
            return Ok(Vec::new());
        }
        limit = limit.min(total - offset);
        let reverse_offset = total - offset - limit;
        // 精确总数与候选页共用快照，尾页可从时间索引另一端读取而不改变页码语义
        if reverse_offset < offset {
            offset = reverse_offset;
            ascending = true;
        }
    }
    let mut statement = QueryBuilder::<Postgres>::new(
        "with candidate_page as materialized (select mr.id, mr.started_at from model_request_observations mr",
    );
    statement.push(" where mr.started_at >= ");
    statement.push_bind(page.range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(page.range.end);
    push_usage_record_status(&mut statement, query.status);
    push_usage_filter(&mut statement, &query.filter, "mr");
    statement.push(if ascending {
        " order by mr.started_at, mr.id limit "
    } else {
        " order by mr.started_at desc, mr.id desc limit "
    });
    statement.push_bind(limit);
    statement.push(" offset ");
    statement.push_bind(offset);
    statement.push(") ");
    statement.push(USAGE_LIST_RECORD_SELECT);
    statement.push(
        " join candidate_page page on page.id = mr.id order by page.started_at desc, page.id desc",
    );
    let rows = statement
        .build()
        .fetch_all(executor)
        .await
        .map_err(|source| postgres_unavailable("list usage records", source))?;
    let items = rows
        .iter()
        .map(usage_list_record_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    Ok(items)
}

// 页码保持全局精确排名，实际明细读取只落在覆盖该页的时间区间
pub(crate) struct TimePage {
    pub(crate) range: ObservabilityRange,
    pub(crate) offset: i64,
    pub(crate) total: Option<u64>,
}

pub(crate) fn time_page_from_counts(
    rows: &[sqlx::postgres::PgRow],
    range: ObservabilityRange,
    offset: i64,
    page_size: ObservabilityPageSize,
) -> StoreResult<(u64, Option<TimePage>)> {
    let counts = rows
        .iter()
        .map(|row| {
            Ok((
                get(row, "bucket_start")?,
                to_u64(get(row, "request_count")?)?,
            ))
        })
        .collect::<StoreResult<Vec<_>>>()?;
    time_page_from_segments(counts, range, offset, page_size, TimeDelta::minutes(1))
}

fn time_page_from_segments(
    counts: impl IntoIterator<Item = (DateTime<Utc>, u64)>,
    range: ObservabilityRange,
    offset: i64,
    page_size: ObservabilityPageSize,
    width: TimeDelta,
) -> StoreResult<(u64, Option<TimePage>)> {
    let offset = u64::try_from(offset).map_err(|_| invalid("negative page offset"))?;
    let end = offset
        .checked_add(u64::from(page_size.get()))
        .ok_or_else(|| invalid("page range overflows u64"))?;
    let mut total = 0_u64;
    let mut page = None;
    let mut selected_count = 0_u64;
    for (bucket, count) in counts {
        let next = total
            .checked_add(count)
            .ok_or_else(|| invalid("request count overflows u64"))?;
        if next > offset && total < end {
            let selected = page.get_or_insert_with(|| TimePage {
                range: ObservabilityRange {
                    start: bucket.max(range.start),
                    end: (bucket + width).min(range.end),
                },
                offset: (offset - total) as i64,
                total: None,
            });
            selected.range.start = bucket.max(range.start);
            selected_count += count;
        }
        total = next;
    }
    if let Some(page) = &mut page {
        page.total = Some(selected_count);
    }
    Ok((total, page))
}

async fn usage_time_page(
    connection: &mut PgConnection,
    query: &UsageRecordQuery,
) -> StoreResult<(u64, Option<TimePage>)> {
    let offset = observability_page_offset(query.current_page, query.page_size)?;
    let mut statement = usage_counts_query(query.range, &query.filter, query.status, true);
    let rows = statement
        .build()
        .fetch_all(connection)
        .await
        .map_err(|source| postgres_unavailable("locate usage page", source))?;
    time_page_from_counts(&rows, query.range, offset, query.page_size)
}

pub(crate) async fn count_usage_records<'e, E: sqlx::Executor<'e, Database = Postgres>>(
    executor: E,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
    status: admin_observability::UsageRecordStatus,
) -> StoreResult<u64> {
    let mut statement = usage_counts_query(range, filter, status, false);
    let total = statement
        .build_query_scalar::<i64>()
        .fetch_one(executor)
        .await
        .map_err(|source| postgres_unavailable("count usage records", source))?;
    to_u64(total)
}

fn usage_counts_query(
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
    status: admin_observability::UsageRecordStatus,
    by_minute: bool,
) -> QueryBuilder<Postgres> {
    use admin_observability::UsageRecordStatus;
    let split = status == UsageRecordStatus::All;
    let mut statement = QueryBuilder::<Postgres>::new(if split {
        if by_minute {
            "select bucket_start,sum(request_count)::bigint as request_count from ("
        } else {
            "select sum(request_count)::bigint as request_count from ("
        }
    } else {
        ""
    });
    // 成功交付和非成功互斥，分别使用交付部分索引和 outcome 索引，保留恢复记录
    for branch in 0..if split { 2 } else { 1 } {
        if branch > 0 {
            statement.push(" union all ");
        }
        statement.push("select ");
        if by_minute {
            statement.push(
                "date_bin(interval '1 minute',mr.started_at,timestamptz 'epoch') as bucket_start,",
            );
        }
        statement.push("count(*)::bigint as request_count from model_request_observations mr where mr.started_at >= ");
        statement.push_bind(range.start);
        statement.push(" and mr.started_at < ");
        statement.push_bind(range.end);
        if branch > 0 {
            // 与 model_requests_outcome_ck 允许的全部非成功状态一致
            statement.push(" and mr.outcome in ('running','failed','cancelled','incomplete')");
        } else {
            push_usage_record_status(
                &mut statement,
                if split {
                    UsageRecordStatus::Success
                } else {
                    status
                },
            );
        }
        if branch > 0 || status == UsageRecordStatus::Error {
            push_non_success_count_filter(&mut statement, filter);
        } else {
            push_usage_filter(&mut statement, filter, "mr");
        }
        if by_minute {
            statement.push(" group by bucket_start");
        }
    }
    if split {
        statement.push(") counts");
        if by_minute {
            statement.push(" group by bucket_start");
        }
    }
    if by_minute {
        statement.push(" order by bucket_start desc");
    }
    statement
}

fn push_non_success_count_filter(
    statement: &mut QueryBuilder<Postgres>,
    filter: &UsageRecordFilter,
) {
    let model_filter = UsageRecordFilter {
        provider_kind: filter.provider_kind.clone(),
        model: filter.model.clone(),
        outcome: filter.outcome.clone(),
        ..Default::default()
    };
    let Some(model) = filter.model.as_ref().filter(|_| *filter == model_filter) else {
        push_usage_filter(statement, filter, "mr");
        return;
    };
    let outcome_only = UsageRecordFilter {
        outcome: filter.outcome.clone(),
        ..Default::default()
    };
    push_usage_filter(statement, &outcome_only, "mr");
    // outcome 索引没有模型或供应方字段；用同一记录的三列等值探测现有索引，避免大批错误回宽表
    // 其他筛选仍需读取源行，继续使用原谓词；两个 EXISTS 的 OR 不会重复计算双模型匹配
    statement.push(" and (");
    for (index, column) in ["requested_model_id", "upstream_model_id"]
        .iter()
        .enumerate()
    {
        if index > 0 {
            statement.push(" or ");
        }
        statement.push(format!(
            "exists(select 1 from model_requests modeled where modeled.{column}="
        ));
        statement.push_bind(model.clone());
        statement.push(" and modeled.started_at=mr.started_at and modeled.id=mr.id");
        if let Some(provider) = &filter.provider_kind {
            // 放在模型候选内，避免外层同表 EXISTS 被合并为 mr.provider_kind 而重新回宽表
            statement.push(
                " and exists(select 1 from model_requests provided where provided.provider_kind=",
            );
            statement.push_bind(provider.clone());
            statement
                .push(" and provided.started_at=modeled.started_at and provided.id=modeled.id)");
        }
        statement.push(")");
    }
    statement.push(")");
}

pub(crate) async fn usage_record_detail(
    connection: &mut PgConnection,
    request_id: &str,
) -> StoreResult<UsageRecordDetail> {
    require_nonempty("model request", "id", request_id)?;
    validate_text(request_id, MAX_FILTER_BYTES, "request ID")?;
    let mut statement = QueryBuilder::<Postgres>::new(USAGE_RECORD_DETAIL_SELECT);
    statement.push(" where mr.id = ");
    statement.push_bind(request_id.to_owned());
    let row = statement
        .build()
        .fetch_optional(&mut *connection)
        .await
        .map_err(|source| postgres_unavailable("load usage record detail", source))?
        .ok_or_else(|| StoreError::NotFound {
            source: None,
            entity: "model request",
            id: request_id.to_owned(),
        })?;
    let request = usage_record_from_row(&row)?;
    let rows = sqlx::query(
        "select id, attempt_index, component, operation,
                provider_kind, provider_account_ref,
                provider_account_name_snapshot as provider_account_name,
                provider_account_email_snapshot as provider_account_email,
                provider_account_authentication_kind_snapshot
                  as provider_account_authentication_kind,
                upstream_model_id,
                failure_kind, status_code, provider_error_code, retry_after_ms,
                upstream_request_id, latency_ms, message, created_at
         from ops_events where model_request_id = $1
         order by attempt_index, created_at, id",
    )
    .bind(request_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(|source| postgres_unavailable("load usage attempt observations", source))?;
    let mut attempts = rows
        .iter()
        .map(intermediate_attempt_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    if request.attempt_count > 0 {
        attempts.push(final_attempt_from_request(&request));
    }
    let trace: Option<Value> = row
        .try_get("diagnostic_trace_json")
        .map_err(|source| postgres_unavailable("decode request trace", source))?;
    let related_requests = sqlx::query_scalar::<_, Value>(
        "select jsonb_build_object('requestId', related.id, 'outcome', related.outcome,
             'relation', case when related.id = current.recovery_request_id then 'recovered_by' else 'recovers' end,
             'completedAt', related.completed_at)
         from model_request_observations current join model_request_observations related
           on related.id = current.recovery_request_id or related.recovery_request_id = current.id
         where current.id = $1 order by related.started_at limit 20"
    ).bind(request_id).fetch_all(&mut *connection).await
        .map_err(|source| postgres_unavailable("load related recovery requests", source))?;
    Ok(UsageRecordDetail {
        request,
        attempts,
        trace,
        related_requests,
    })
}

pub(crate) fn intermediate_attempt_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<UsageAttemptObservation> {
    Ok(UsageAttemptObservation {
        source: "ops_event".to_owned(),
        id: get(row, "id")?,
        attempt_index: to_u32(get(row, "attempt_index")?)?,
        component: get(row, "component")?,
        operation: get(row, "operation")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        provider_account_authentication_kind: get(row, "provider_account_authentication_kind")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: None,
        upstream_send_state: None,
        outcome: admin_observability::RequestOutcome::Failed,
        downstream_committed: None,
        status_code: optional_status(row, "status_code")?,
        provider_error_code: get(row, "provider_error_code")?,
        failure_kind: get(row, "failure_kind")?,
        retry_after_ms: optional_unsigned(row, "retry_after_ms")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        message: get(row, "message")?,
        input_tokens: None,
        output_tokens: None,
        cached_input_tokens: None,
        cache_write_input_tokens: None,
        reasoning_output_tokens: None,
        total_tokens: None,
        cost_source: None,
        cost_amount: None,
        cost_currency: None,
        occurred_at: get(row, "created_at")?,
    })
}

pub(crate) fn final_attempt_from_request(request: &UsageRecord) -> UsageAttemptObservation {
    UsageAttemptObservation {
        source: "model_request".to_owned(),
        id: format!("{}:final", request.id),
        attempt_index: request.attempt_count,
        component: "model_request".to_owned(),
        operation: request.operation.clone(),
        provider_kind: request.provider_kind.clone(),
        provider_account_ref: request.provider_account_ref.clone(),
        provider_account_name: request.provider_account_name.clone(),
        provider_account_email: request.provider_account_email.clone(),
        provider_account_authentication_kind: request.provider_account_authentication_kind.clone(),
        upstream_model_id: request.upstream_model_id.clone(),
        upstream_transport: request.upstream_transport.clone(),
        upstream_send_state: Some(request.upstream_send_state.clone()),
        outcome: request.outcome.clone(),
        downstream_committed: Some(request.downstream_committed_at.is_some()),
        status_code: request.upstream_status_code.or(request.client_status_code),
        provider_error_code: request.provider_error_code.clone(),
        failure_kind: request.error_kind.clone(),
        retry_after_ms: request.retry_after_ms,
        upstream_request_id: request.upstream_request_id.clone(),
        latency_ms: request.latency_ms,
        message: request.error_message.clone(),
        input_tokens: request.input_tokens,
        output_tokens: request.output_tokens,
        cached_input_tokens: request.cached_input_tokens,
        cache_write_input_tokens: request.cache_write_input_tokens,
        reasoning_output_tokens: request.reasoning_output_tokens,
        total_tokens: request.total_tokens,
        cost_source: Some(request.cost_source.clone()),
        cost_amount: request.cost_amount.clone(),
        cost_currency: request.cost_currency.clone(),
        occurred_at: request.completed_at.unwrap_or(request.started_at),
    }
}

pub(crate) fn push_diagnostic_dimension_filter(
    statement: &mut QueryBuilder<Postgres>,
    dimension: DiagnosticDimension,
) {
    match dimension {
        DiagnosticDimension::Failure => {
            statement.push(" and mr.error_kind is not null");
        }
        DiagnosticDimension::Model => {
            statement.push(" and ");
            statement.push(MODEL_DIAGNOSTIC_DIMENSION_SQL);
            statement.push(" is not null");
        }
        DiagnosticDimension::Status => {
            statement
                .push(" and coalesce(mr.upstream_status_code, mr.client_status_code) is not null");
        }
        DiagnosticDimension::Provider
        | DiagnosticDimension::Account
        | DiagnosticDimension::ApiKey
        | DiagnosticDimension::Transport => {}
    }
}

fn push_usage_record_status(
    statement: &mut QueryBuilder<Postgres>,
    status: admin_observability::UsageRecordStatus,
) {
    use admin_observability::UsageRecordStatus;
    match status {
        UsageRecordStatus::Success => push_completed_usage_fact_filter(statement, "mr"),
        UsageRecordStatus::Error => {
            statement.push(" and mr.outcome in ('failed', 'incomplete')");
        }
        UsageRecordStatus::All => {
            // 成功沿用用量边界，未成功记录保留真实生命周期，不把取消与执行中标为错误
            statement
                .push(" and (")
                .push(completed_usage_fact_predicate("mr"))
                .push(" or mr.outcome <> 'succeeded')");
        }
    }
}
