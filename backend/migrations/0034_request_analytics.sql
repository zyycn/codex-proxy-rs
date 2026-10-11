-- 请求观测的可重建窄事实、持久化工作队列与分层统计
create sequence request_analytics_change_version;
create table request_analytics_facts as select
    mr.id, mr.started_at, mr.completed_at, mr.client_api_key_ref, mr.provider_account_ref, mr.routing_group_refs, mr.operation, mr.provider_kind, mr.requested_model_id, mr.upstream_model_id, mr.outcome, mr.client_status_code, mr.upstream_status_code, mr.client_transport, mr.upstream_transport, mr.attempt_count, mr.client_response_id, mr.upstream_response_id, mr.upstream_request_id, mr.recovered_at, mr.downstream_committed_at, mr.request_kind, mr.image_generation_requested, mr.image_generation_succeeded, mr.error_kind, mr.input_tokens, mr.output_tokens, mr.cached_input_tokens, mr.cache_write_input_tokens, mr.reasoning_output_tokens, mr.image_input_tokens, mr.image_output_tokens, mr.total_tokens, mr.cost_source, mr.cost_currency, mr.cost_amount, mr.first_token_ms, mr.latency_ms, mr.admission_decision_ms, mr.account_selection_wait_ms, mr.upstream_response_ms, mr.capacity_used_slots, mr.capacity_total_slots
from model_request_observations mr with no data;
alter table request_analytics_facts add primary key (id),
    add column standard_cost_amount numeric(20,10),
    add column no_cache_cost_amount numeric(20,10),
    add column source_version bigint not null,
    add column schema_version integer not null default 1,
    add column ops_failure_count bigint not null default 0,
    add column ops_rate_limited_count bigint not null default 0,
    add column ops_auth_failure_count bigint not null default 0,
    add column ops_provider_5xx_count bigint not null default 0;
create index request_analytics_facts_started on request_analytics_facts (started_at, id);
create index request_analytics_facts_key on request_analytics_facts (client_api_key_ref, started_at, id);
create index request_analytics_facts_account on request_analytics_facts (provider_account_ref, started_at, id);
-- 分组集合未预聚合，窄事实回退仍需按集合定位，避免低命中筛选扫描全部保留历史
create index request_analytics_facts_groups on request_analytics_facts using gin (routing_group_refs);
-- 用量列表的精确总数与账号最近使用只需要交付事实，部分索引避免为判定资格读取宽行
create index model_requests_ops_completed on model_requests (completed_at,id)
where error_kind is not null and error_kind <> 'cancelled';
create index model_requests_delivered_started on model_requests (started_at,id)
where outcome='succeeded' and downstream_committed_at is not null
    and (provider_kind is distinct from 'openai' or request_kind is distinct from 'prewarm')
    and ((client_transport='websocket' and client_status_code is null) or client_status_code between 200 and 399)
    and (requested_model_id is not null or upstream_model_id is not null or image_generation_requested
         or input_tokens is not null or output_tokens is not null or cached_input_tokens is not null
         or cache_write_input_tokens is not null or reasoning_output_tokens is not null
         or image_input_tokens is not null or image_output_tokens is not null
         or total_tokens is not null or cost_amount is not null);
create index request_analytics_facts_delivered_account on request_analytics_facts (provider_account_ref,started_at desc,id desc)
where outcome='succeeded' and downstream_committed_at is not null
    and (provider_kind is distinct from 'openai' or request_kind is distinct from 'prewarm')
    and ((client_transport='websocket' and client_status_code is null) or client_status_code between 200 and 399)
    and (requested_model_id is not null or upstream_model_id is not null or image_generation_requested
         or input_tokens is not null or output_tokens is not null or cached_input_tokens is not null
         or cache_write_input_tokens is not null or reasoning_output_tokens is not null
         or image_input_tokens is not null or image_output_tokens is not null
         or total_tokens is not null or cost_amount is not null);
-- 不透明 ID 没有私有长度上限，等值 HASH 索引避免长值超过 B-tree 索引项限制
create index request_analytics_facts_client_response on request_analytics_facts using hash (client_response_id) where client_response_id is not null;
create index request_analytics_facts_upstream_response on request_analytics_facts using hash (upstream_response_id) where upstream_response_id is not null;
create index request_analytics_facts_upstream_request on request_analytics_facts using hash (upstream_request_id) where upstream_request_id is not null;
create table request_analytics_pending_requests (
    request_id text primary key,
    change_version bigint not null default nextval('request_analytics_change_version'),
    oldest_pending_at timestamptz not null default clock_timestamp(),
    previous_bucket timestamptz,
    deletion_pending boolean not null default false
);
create index request_analytics_pending_deletions on request_analytics_pending_requests (previous_bucket) where deletion_pending;
create index request_analytics_pending_requests_age on request_analytics_pending_requests (oldest_pending_at, request_id);
create table request_analytics_pending_buckets (
    bucket_start timestamptz primary key,
    change_version bigint not null default nextval('request_analytics_change_version'),
    oldest_pending_at timestamptz not null,
    build_version bigint,
    build_phase smallint not null default 0 check (build_phase between 0 and 4),
    build_started_at timestamptz,
    build_id text,
    build_collection text,
    build_dimensions text[],
    build_outcome text
);
create index request_analytics_pending_buckets_age on request_analytics_pending_buckets(oldest_pending_at,bucket_start);
create table request_analytics_15m (
    bucket_start timestamptz not null,
    collection text not null,
    dimensions text[] not null,
    outcome text not null,
    schema_version integer not null,
    histogram_version integer not null,
    counts jsonb not null,
    request_count bigint generated always as ((counts->>'request_count')::bigint) stored,
    costs jsonb not null,
    distributions jsonb not null,
    primary key (bucket_start, collection, dimensions, outcome)
);
create table request_analytics_hourly (like request_analytics_15m including all);
create index request_analytics_15m_collection on request_analytics_15m (collection,bucket_start);
create index request_analytics_hourly_collection on request_analytics_hourly (collection,bucket_start);
create index request_analytics_15m_dimension on request_analytics_15m (collection,(dimensions[1]),bucket_start);
create index request_analytics_hourly_dimension on request_analytics_hourly (collection,(dimensions[1]),bucket_start);
-- 排名只读取确定派生的计数，覆盖索引避免为高基数账号和密钥读取宽分布行
create index request_analytics_15m_rank on request_analytics_15m (collection,bucket_start) include (dimensions,outcome,schema_version,histogram_version,request_count) where collection in ('account','key');
create index request_analytics_hourly_rank on request_analytics_hourly (collection,bucket_start) include (dimensions,outcome,schema_version,histogram_version,request_count) where collection in ('account','key');
-- 高频身份排名只保存日级精确计数，不复制分布与诊断详情
create table request_analytics_daily_counts (
    bucket_start timestamptz not null,
    collection text not null check (collection in ('account','key')),
    dimension text not null,
    outcome text not null,
    schema_version integer not null,
    histogram_version integer not null,
    request_count bigint not null check (request_count >= 0),
    primary key (collection,bucket_start,dimension,outcome,schema_version,histogram_version)
);

-- 计数由已持久化小时行的 OLD/NEW 差额派生，重放同值不会重复累计
-- 未发布小时所在的日期不参与日级查询，分段重建与版本清理仍可逐批提交
create function sync_request_analytics_daily_counts() returns trigger language plpgsql as $$
declare
    old_day timestamptz;
    new_day timestamptz;
    delta bigint;
begin
    if TG_OP <> 'INSERT' then
        old_day := date_bin(interval '1 day',OLD.bucket_start,timestamptz '2000-01-01 UTC');
    end if;
    if TG_OP <> 'DELETE' then
        new_day := date_bin(interval '1 day',NEW.bucket_start,timestamptz '2000-01-01 UTC');
    end if;
    if TG_OP = 'UPDATE' and
        (old_day,OLD.collection,OLD.dimensions[1],OLD.outcome,OLD.schema_version,OLD.histogram_version)
        is not distinct from
        (new_day,NEW.collection,NEW.dimensions[1],NEW.outcome,NEW.schema_version,NEW.histogram_version) then
        if NEW.collection in ('account','key') then
            delta := NEW.request_count - OLD.request_count;
            if delta <> 0 then
                update request_analytics_daily_counts set request_count=request_count+delta
                where collection=NEW.collection and bucket_start=new_day and dimension=NEW.dimensions[1]
                    and outcome=NEW.outcome and schema_version=NEW.schema_version and histogram_version=NEW.histogram_version;
            end if;
        end if;
        return null;
    end if;
    if TG_OP <> 'INSERT' and OLD.collection in ('account','key') then
        update request_analytics_daily_counts set request_count=request_count-OLD.request_count
        where collection=OLD.collection and bucket_start=old_day and dimension=OLD.dimensions[1]
            and outcome=OLD.outcome and schema_version=OLD.schema_version and histogram_version=OLD.histogram_version;
        delete from request_analytics_daily_counts
        where collection=OLD.collection and bucket_start=old_day and dimension=OLD.dimensions[1]
            and outcome=OLD.outcome and schema_version=OLD.schema_version and histogram_version=OLD.histogram_version
            and request_count=0;
    end if;
    if TG_OP <> 'DELETE' and NEW.collection in ('account','key') then
        insert into request_analytics_daily_counts values(new_day,NEW.collection,NEW.dimensions[1],NEW.outcome,NEW.schema_version,NEW.histogram_version,NEW.request_count)
        on conflict(collection,bucket_start,dimension,outcome,schema_version,histogram_version)
        do update set request_count=request_analytics_daily_counts.request_count+excluded.request_count;
    end if;
    return null;
end;
$$;
create trigger request_analytics_hourly_daily_counts
    after insert or update or delete on request_analytics_hourly
    for each row execute function sync_request_analytics_daily_counts();

create table request_analytics_build_state (
    singleton boolean primary key default true check (singleton),
    schema_version integer not null default 1,
    histogram_version integer not null default 1,
    coverage_start timestamptz,
    coverage_end timestamptz,
    backfill_cursor_started_at timestamptz,
    backfill_cursor_id text,
    backfill_complete boolean not null default false,
    reset_phase smallint not null default 0 check (reset_phase between 0 and 4),
    reset_cursor jsonb,
    generation bigint not null default 0,
    updated_at timestamptz not null default clock_timestamp(),
    check (reset_phase <> 0 or reset_cursor is null)
);
insert into request_analytics_build_state (singleton) values (true);

create function enqueue_request_analytics(request_ref text, request_start timestamptz, source_deleted boolean default false)
returns void language sql as $$
    insert into request_analytics_pending_requests(request_id, previous_bucket, deletion_pending)
    select request_ref, date_bin(interval '15 minutes', coalesce(request_start,(select started_at from request_analytics_facts where id=request_ref)), timestamptz '2000-01-01 UTC'), source_deleted
    where request_ref is not null
    on conflict (request_id) do update
    set change_version = nextval('request_analytics_change_version'),
        previous_bucket = coalesce(request_analytics_pending_requests.previous_bucket, excluded.previous_bucket),
        deletion_pending = request_analytics_pending_requests.deletion_pending or excluded.deletion_pending
$$;
create function observe_request_analytics_change() returns trigger language plpgsql as $$
begin
    if TG_OP = 'DELETE' then
        perform enqueue_request_analytics(OLD.id, OLD.started_at, true);
        return OLD;
    end if;
    perform enqueue_request_analytics(NEW.id, NEW.started_at);
    return NEW;
end
$$;
create trigger request_analytics_insert after insert on model_requests
for each row execute function observe_request_analytics_change();
create trigger request_analytics_delete after delete on model_requests
for each row execute function observe_request_analytics_change();
create trigger request_analytics_update after update on model_requests
for each row when (ROW(OLD.id, OLD.started_at, OLD.completed_at, OLD.client_api_key_ref, OLD.provider_account_ref, OLD.operation, OLD.provider_kind, OLD.requested_model_id, OLD.upstream_model_id, OLD.outcome, OLD.client_status_code, OLD.upstream_status_code, OLD.client_transport, OLD.upstream_transport, OLD.attempt_count, OLD.client_response_id, OLD.upstream_response_id, OLD.upstream_request_id, OLD.recovered_at, OLD.downstream_committed_at, OLD.request_kind, OLD.image_generation_requested, OLD.image_generation_succeeded, OLD.error_kind, OLD.input_tokens, OLD.output_tokens, OLD.cached_input_tokens, OLD.cache_write_input_tokens, OLD.reasoning_output_tokens, OLD.image_input_tokens, OLD.image_output_tokens, OLD.total_tokens, OLD.cost_source, OLD.cost_currency, OLD.cost_amount) is distinct from ROW(NEW.id, NEW.started_at, NEW.completed_at, NEW.client_api_key_ref, NEW.provider_account_ref, NEW.operation, NEW.provider_kind, NEW.requested_model_id, NEW.upstream_model_id, NEW.outcome, NEW.client_status_code, NEW.upstream_status_code, NEW.client_transport, NEW.upstream_transport, NEW.attempt_count, NEW.client_response_id, NEW.upstream_response_id, NEW.upstream_request_id, NEW.recovered_at, NEW.downstream_committed_at, NEW.request_kind, NEW.image_generation_requested, NEW.image_generation_succeeded, NEW.error_kind, NEW.input_tokens, NEW.output_tokens, NEW.cached_input_tokens, NEW.cache_write_input_tokens, NEW.reasoning_output_tokens, NEW.image_input_tokens, NEW.image_output_tokens, NEW.total_tokens, NEW.cost_source, NEW.cost_currency, NEW.cost_amount)
    or OLD.billing_snapshot_json is distinct from NEW.billing_snapshot_json
    or OLD.service_tier is distinct from NEW.service_tier
    or OLD.request_observation_json->'timings' is distinct from NEW.request_observation_json->'timings'
    or OLD.request_observation_json->'scheduling' is distinct from NEW.request_observation_json->'scheduling'
    or OLD.request_observation_json->'routing' is distinct from NEW.request_observation_json->'routing')
execute function observe_request_analytics_change();
create function observe_ops_analytics_change() returns trigger language plpgsql as $$
begin
    if TG_OP <> 'INSERT' then
        perform enqueue_request_analytics(OLD.model_request_id, null, TG_OP = 'DELETE');
    end if;
    if TG_OP <> 'DELETE' then
        perform enqueue_request_analytics(NEW.model_request_id, null);
        return NEW;
    end if;
    return OLD;
end
$$;
create trigger request_analytics_ops_insert after insert on ops_events
for each row execute function observe_ops_analytics_change();
create trigger request_analytics_ops_delete after delete on ops_events
for each row execute function observe_ops_analytics_change();
create trigger request_analytics_ops_update after update on ops_events
for each row when (ROW(OLD.model_request_id, OLD.failure_kind, OLD.status_code, OLD.attempt_index)
    is distinct from ROW(NEW.model_request_id, NEW.failure_kind, NEW.status_code, NEW.attempt_index))
execute function observe_ops_analytics_change();

-- 删除在源事务内使缓存失效，语句级触发避免批量清理争用全局行
create function invalidate_deleted_request_analytics() returns trigger language plpgsql as $$
begin
    update request_analytics_build_state set generation = generation + 1 where singleton;
    return null;
end
$$;
create trigger request_analytics_delete_invalidation after delete on model_requests
for each statement execute function invalidate_deleted_request_analytics();
create function invalidate_deleted_ops_analytics() returns trigger language plpgsql as $$
begin
    -- 父请求删除已经负责失效；独立删除仍有关联源请求的 Ops 才需要另行失效
    if exists (select 1 from deleted_ops join model_requests mr on mr.id = deleted_ops.model_request_id) then
        update request_analytics_build_state set generation = generation + 1 where singleton;
    end if;
    return null;
end
$$;
create trigger request_analytics_ops_delete_invalidation after delete on ops_events
referencing old table as deleted_ops
for each statement execute function invalidate_deleted_ops_analytics();
