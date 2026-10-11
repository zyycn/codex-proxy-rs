-- 结构校验只约束路由字段类型，枚举范围由同名业务约束负责
create or replace function model_request_observation_valid(document jsonb) returns boolean
language plpgsql immutable parallel safe as $$
declare
  spec record;
  item jsonb;
  number numeric;
begin
  if document is null or jsonb_typeof(document) <> 'object'
     or octet_length(document::text) > 1048576 then
    return false;
  end if;
  for spec in select * from (values
    ('{}'::text[], array['account','continuation','error','recovery','request','response','routing','scheduling','timings','transport']::text[]),
    ('{request}'::text[], array['clientIp','compact','configRevision','endpoint','protocol','reasoningEffort','reasoningPreset','subagentKind','userAgent']::text[]),
    ('{account}'::text[], array['authenticationKind','email','name']::text[]),
    ('{routing}'::text[], array['groupNamesSnapshot','groupRefs','scope']::text[]),
    ('{error}'::text[], array['message','providerErrorCode','retryAfterMs']::text[]),
    ('{timings}'::text[], array['local','upstream']::text[]),
    ('{timings,local}'::text[], array['connectMs','firstEventMs','firstReasoningMs','firstTextMs','firstTokenMs','headersMs','latencyMs','transportDecisionWaitMs']::text[]),
    ('{timings,upstream}'::text[], array['processingMs','responseMs','apiOverheadMs','engineMs','engineIapiTtftMs','engineServiceTtftMs','engineIapiTbtMs','engineServiceTbtMs']::text[]),
    ('{scheduling}'::text[], array['accountSelectionWaitMs','admissionDecisionMs','capacityTotalSlots','capacityUsedSlots']::text[]),
    ('{transport}'::text[], array['connection','httpVersion','websocketPool']::text[]),
    ('{transport,connection}'::text[], array['ageMs','exitReason','id','idleMs']::text[]),
    ('{response}'::text[], array['responseModel']::text[]),
    ('{continuation}'::text[], array['previousResponseIdHash','unavailableReason']::text[]),
    ('{recovery}'::text[], array['retryDelayMs','totalLatencyMs']::text[])) as groups(path, allowed_keys) loop
    item := document #> spec.path;
    if item is not null and (jsonb_typeof(item) <> 'object' or item - spec.allowed_keys <> '{}'::jsonb) then
      return false;
    end if;
  end loop;
  for spec in select * from (values
    ('{request,configRevision}'::text[], 'number'),
    ('{request,protocol}'::text[], 'string'),
    ('{request,endpoint}'::text[], 'string'),
    ('{account,name}'::text[], 'string'),
    ('{account,email}'::text[], 'string'),
    ('{account,authenticationKind}'::text[], 'string'),
    ('{routing,scope}'::text[], 'string'),
    ('{routing,groupRefs}'::text[], 'array'),
    ('{routing,groupNamesSnapshot}'::text[], 'array'),
    ('{error,providerErrorCode}'::text[], 'string'),
    ('{error,message}'::text[], 'string'),
    ('{error,retryAfterMs}'::text[], 'number'),
    ('{request,clientIp}'::text[], 'string'),
    ('{request,userAgent}'::text[], 'string'),
    ('{request,reasoningEffort}'::text[], 'string'),
    ('{request,reasoningPreset}'::text[], 'string'),
    ('{request,subagentKind}'::text[], 'string'),
    ('{request,compact}'::text[], 'boolean'),
    ('{timings,local,transportDecisionWaitMs}'::text[], 'number'),
    ('{timings,local,connectMs}'::text[], 'number'),
    ('{timings,local,headersMs}'::text[], 'number'),
    ('{timings,local,firstEventMs}'::text[], 'number'),
    ('{timings,local,firstReasoningMs}'::text[], 'number'),
    ('{timings,local,firstTextMs}'::text[], 'number'),
    ('{timings,local,firstTokenMs}'::text[], 'number'),
    ('{timings,local,latencyMs}'::text[], 'number'),
    ('{timings,upstream,processingMs}'::text[], 'number'),
    ('{timings,upstream,apiOverheadMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineIapiTtftMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineServiceTtftMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineIapiTbtMs}'::text[], 'milliseconds'),
    ('{timings,upstream,engineServiceTbtMs}'::text[], 'milliseconds'),
    ('{scheduling,admissionDecisionMs}'::text[], 'number'),
    ('{scheduling,accountSelectionWaitMs}'::text[], 'number'),
    ('{scheduling,capacityUsedSlots}'::text[], 'number'),
    ('{scheduling,capacityTotalSlots}'::text[], 'number'),
    ('{transport,httpVersion}'::text[], 'string'),
    ('{transport,websocketPool}'::text[], 'string'),
    ('{transport,connection,id}'::text[], 'string'),
    ('{transport,connection,exitReason}'::text[], 'string'),
    ('{transport,connection,ageMs}'::text[], 'number'),
    ('{transport,connection,idleMs}'::text[], 'number'),
    ('{response,responseModel}'::text[], 'string'),
    ('{continuation,previousResponseIdHash}'::text[], 'string'),
    ('{continuation,unavailableReason}'::text[], 'string'),
    ('{recovery,retryDelayMs}'::text[], 'number'),
    ('{recovery,totalLatencyMs}'::text[], 'number'),
    ('{timings,upstream,responseMs}'::text[], 'number')) as fields(path, kind) loop
    item := document #> spec.path;
    if item is null or item = 'null'::jsonb then continue; end if;
    if spec.kind = 'milliseconds' then
      if jsonb_typeof(item) <> 'number' then return false; end if;
      number := (item #>> '{}')::numeric;
      if number < 0 or number > 9223372036854775807 then return false; end if;
      continue;
    end if;
    if jsonb_typeof(item) <> spec.kind then return false; end if;
    if spec.kind = 'number' then
      number := (item #>> '{}')::numeric;
      if item #>> '{}' !~ '^[0-9]+$' or number > 9223372036854775807 then return false; end if;
    elsif spec.kind = 'array' then
      if exists (select 1 from jsonb_array_elements(item) member where jsonb_typeof(member) <> 'string') then return false; end if;
    end if;
  end loop;
  if (document #> '{request,configRevision}' is not null
          and document #> '{request,configRevision}' <> 'null'::jsonb
          and document #> '{request,compact}' in ('true'::jsonb, 'false'::jsonb)
          and coalesce(length(document #>> '{request,protocol}'), 0) > 0
          and coalesce(length(document #>> '{request,endpoint}'), 0) > 0
          and jsonb_typeof(document #> '{routing,scope}') = 'string'
          and jsonb_typeof(document #> '{routing,groupRefs}') = 'array'
          and jsonb_typeof(document #> '{routing,groupNamesSnapshot}') = 'array') is not true then
    return false;
  end if;
  if document #>> '{request,clientIp}' is not null then
    perform (document #>> '{request,clientIp}')::inet;
  end if;
  return true;
exception when invalid_text_representation or numeric_value_out_of_range then
  return false;
end;
$$;

-- 按最终行的真实依赖执行校验，纯状态写入不展开无关 JSON
create or replace function check_model_request_observation() returns trigger
language plpgsql as $$
declare
  json_changed boolean := tg_op = 'INSERT' or old.request_observation_json is distinct from new.request_observation_json;
begin
  if json_changed then
    if model_request_observation_valid(new.request_observation_json) is not true then
      raise exception using errcode = '23514', constraint = 'model_requests_observation_shape_ck',
        message = 'request observation violates shape constraint', table = tg_table_name, schema = tg_table_schema;
    end if;
  end if;
  if json_changed then
    declare
      timings_upstream_response_ms bigint;
    begin
      timings_upstream_response_ms := (new.request_observation_json #>> '{timings,upstream,responseMs}')::bigint;
      if (timings_upstream_response_ms > 0) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_upstream_duration_ck',
          message = 'request observation violates model_requests_upstream_duration_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed then
    if ((new.request_observation_json #>> '{request,configRevision}')::bigint > 0) is false then
      raise exception using errcode = '23514', constraint = 'model_requests_revision_attempt_ck', message = 'request observation violates model_requests_revision_attempt_ck', table = tg_table_name, schema = tg_table_schema;
    end if;
  end if;
  if json_changed then
    if ((new.request_observation_json #>> '{error,retryAfterMs}')::bigint is null or (new.request_observation_json #>> '{error,retryAfterMs}')::bigint >= 0) is false then
      raise exception using errcode = '23514', constraint = 'model_requests_status_ck', message = 'request observation violates model_requests_status_ck', table = tg_table_name, schema = tg_table_schema;
    end if;
  end if;
  if json_changed then
    declare
      transport_websocket_pool text;
    begin
      transport_websocket_pool := (new.request_observation_json #>> '{transport,websocketPool}');
      if (transport_websocket_pool is null or transport_websocket_pool in ('new', 'reuse')) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_websocket_pool_ck',
          message = 'request observation violates model_requests_websocket_pool_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed then
    declare
      routing_group_names_snapshot jsonb;
      routing_scope text;
      routing_group_refs jsonb;
    begin
      routing_group_names_snapshot := (new.request_observation_json #> '{routing,groupNamesSnapshot}');
      routing_scope := (new.request_observation_json #>> '{routing,scope}');
      routing_group_refs := (new.request_observation_json #> '{routing,groupRefs}');
      if (jsonb_typeof(routing_group_names_snapshot) = 'array'
    and (
      (
        routing_scope in ('legacy_provider', 'all')
        and jsonb_array_length(routing_group_refs) = 0
        and jsonb_array_length(routing_group_names_snapshot) = 0
      )
      or (
        routing_scope = 'groups'
        and jsonb_array_length(routing_group_refs) > 0
        and not jsonb_path_exists(new.request_observation_json, '$.routing.groupRefs[*] ? (@ == null)')
        and jsonb_array_length(routing_group_names_snapshot)
          = jsonb_array_length(routing_group_refs)
      )
    )) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_routing_group_names_ck',
          message = 'request observation violates model_requests_routing_group_names_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed then
    declare
      routing_scope text;
    begin
      routing_scope := (new.request_observation_json #>> '{routing,scope}');
      if (routing_scope in ('legacy_provider', 'all', 'groups')) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_routing_scope_ck',
          message = 'request observation violates model_requests_routing_scope_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed then
    declare
      timings_local_transport_decision_wait_ms bigint;
      timings_local_connect_ms bigint;
      timings_local_headers_ms bigint;
      timings_local_first_event_ms bigint;
      timings_local_first_reasoning_ms bigint;
      timings_local_first_text_ms bigint;
      timings_local_first_token_ms bigint;
      timings_upstream_processing_ms bigint;
      timings_local_latency_ms bigint;
    begin
      timings_local_transport_decision_wait_ms := (new.request_observation_json #>> '{timings,local,transportDecisionWaitMs}')::bigint;
      timings_local_connect_ms := (new.request_observation_json #>> '{timings,local,connectMs}')::bigint;
      timings_local_headers_ms := (new.request_observation_json #>> '{timings,local,headersMs}')::bigint;
      timings_local_first_event_ms := (new.request_observation_json #>> '{timings,local,firstEventMs}')::bigint;
      timings_local_first_reasoning_ms := (new.request_observation_json #>> '{timings,local,firstReasoningMs}')::bigint;
      timings_local_first_text_ms := (new.request_observation_json #>> '{timings,local,firstTextMs}')::bigint;
      timings_local_first_token_ms := (new.request_observation_json #>> '{timings,local,firstTokenMs}')::bigint;
      timings_upstream_processing_ms := (new.request_observation_json #>> '{timings,upstream,processingMs}')::bigint;
      timings_local_latency_ms := (new.request_observation_json #>> '{timings,local,latencyMs}')::bigint;
      if ((timings_local_transport_decision_wait_ms is null or timings_local_transport_decision_wait_ms >= 0)
    and (timings_local_connect_ms is null or timings_local_connect_ms >= 0)
    and (timings_local_headers_ms is null or timings_local_headers_ms >= 0)
    and (timings_local_first_event_ms is null or timings_local_first_event_ms >= 0)
    and (timings_local_first_reasoning_ms is null or timings_local_first_reasoning_ms >= 0)
    and (timings_local_first_text_ms is null or timings_local_first_text_ms >= 0)
    and (timings_local_first_token_ms is null or timings_local_first_token_ms >= 0)
    and (timings_upstream_processing_ms is null or timings_upstream_processing_ms >= 0)
    and (timings_local_latency_ms is null or timings_local_latency_ms >= 0)
    and (
      timings_local_latency_ms is null
      or (
        (timings_local_transport_decision_wait_ms is null or timings_local_transport_decision_wait_ms <= timings_local_latency_ms)
        and (timings_local_connect_ms is null or timings_local_connect_ms <= timings_local_latency_ms)
        and (timings_local_headers_ms is null or timings_local_headers_ms <= timings_local_latency_ms)
        and (timings_local_first_event_ms is null or timings_local_first_event_ms <= timings_local_latency_ms)
        and (timings_local_first_reasoning_ms is null or timings_local_first_reasoning_ms <= timings_local_latency_ms)
        and (timings_local_first_text_ms is null or timings_local_first_text_ms <= timings_local_latency_ms)
        and (timings_local_first_token_ms is null or timings_local_first_token_ms <= timings_local_latency_ms)
      )
    )) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_latency_ck',
          message = 'request observation violates model_requests_latency_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed then
    declare
      scheduling_admission_decision_ms bigint;
      scheduling_account_selection_wait_ms bigint;
      scheduling_capacity_used_slots bigint;
      scheduling_capacity_total_slots bigint;
    begin
      scheduling_admission_decision_ms := (new.request_observation_json #>> '{scheduling,admissionDecisionMs}')::bigint;
      scheduling_account_selection_wait_ms := (new.request_observation_json #>> '{scheduling,accountSelectionWaitMs}')::bigint;
      scheduling_capacity_used_slots := (new.request_observation_json #>> '{scheduling,capacityUsedSlots}')::bigint;
      scheduling_capacity_total_slots := (new.request_observation_json #>> '{scheduling,capacityTotalSlots}')::bigint;
      if ((scheduling_admission_decision_ms is null or scheduling_admission_decision_ms >= 0)
    and (scheduling_account_selection_wait_ms is null or scheduling_account_selection_wait_ms >= 0)
    and (
      (scheduling_capacity_used_slots is null and scheduling_capacity_total_slots is null)
      or (
        scheduling_capacity_used_slots >= 0
        and scheduling_capacity_total_slots > 0
        and scheduling_capacity_used_slots <= scheduling_capacity_total_slots
      )
    )) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_runtime_pressure_ck',
          message = 'request observation violates model_requests_runtime_pressure_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed or old.continuation_requested is distinct from new.continuation_requested then
    if (((new.request_observation_json #>> '{continuation,previousResponseIdHash}') is null or (new.request_observation_json #>> '{continuation,previousResponseIdHash}') ~ '^[0-9a-f]{64}$') and (new.continuation_requested or (new.request_observation_json #>> '{continuation,previousResponseIdHash}') is null)) is false then
      raise exception using errcode = '23514', constraint = 'model_requests_continuation_hashes_ck', message = 'request observation violates model_requests_continuation_hashes_ck', table = tg_table_name, schema = tg_table_schema;
    end if;
  end if;
  if json_changed then
    declare
      continuation_unavailable_reason text;
    begin
      continuation_unavailable_reason := (new.request_observation_json #>> '{continuation,unavailableReason}');
      if (continuation_unavailable_reason is null
    or (
      octet_length(continuation_unavailable_reason) between 1 and 64
      and continuation_unavailable_reason ~ '^[a-z][a-z0-9_]*$'
    )) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_continuation_reason_ck',
          message = 'request observation violates model_requests_continuation_reason_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed then
    declare
      transport_connection_id text;
      transport_connection_exit_reason text;
      transport_connection_age_ms bigint;
      transport_connection_idle_ms bigint;
    begin
      transport_connection_id := (new.request_observation_json #>> '{transport,connection,id}');
      transport_connection_exit_reason := (new.request_observation_json #>> '{transport,connection,exitReason}');
      transport_connection_age_ms := (new.request_observation_json #>> '{transport,connection,ageMs}')::bigint;
      transport_connection_idle_ms := (new.request_observation_json #>> '{transport,connection,idleMs}')::bigint;
      if ((
      transport_connection_id is null
      and transport_connection_exit_reason is null
      and transport_connection_age_ms is null
      and transport_connection_idle_ms is null
    )
    or (
      octet_length(transport_connection_id) between 1 and 128
      and transport_connection_id !~ '[[:cntrl:]]'
      and octet_length(transport_connection_exit_reason) between 1 and 64
      and transport_connection_exit_reason ~ '^[a-z][a-z0-9_]*$'
      and transport_connection_age_ms >= 0
      and transport_connection_idle_ms between 0 and transport_connection_age_ms
    )) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_upstream_connection_ck',
          message = 'request observation violates model_requests_upstream_connection_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed or old.completed_at is distinct from new.completed_at or old.recovered_at is distinct from new.recovered_at or old.recovery_attempt_count is distinct from new.recovery_attempt_count or old.recovery_request_id is distinct from new.recovery_request_id then
    declare
      recovery_retry_delay_ms bigint;
      recovery_total_latency_ms bigint;
    begin
      recovery_retry_delay_ms := (new.request_observation_json #>> '{recovery,retryDelayMs}')::bigint;
      recovery_total_latency_ms := (new.request_observation_json #>> '{recovery,totalLatencyMs}')::bigint;
      if (new.recovery_attempt_count >= 0
    and (
      (
        new.recovered_at is null
        and new.recovery_request_id is null
        and recovery_retry_delay_ms is null
        and recovery_total_latency_ms is null
      )
      or (
        new.recovered_at is not null
        and new.recovered_at >= new.completed_at
        and new.recovery_request_id is not null
        and octet_length(new.recovery_request_id) between 1 and 128
        and new.recovery_attempt_count > 0
        and recovery_retry_delay_ms >= 0
        and recovery_total_latency_ms >= recovery_retry_delay_ms
      )
    )) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_recovery_ck',
          message = 'request observation violates model_requests_recovery_ck', table = tg_table_name, schema = tg_table_schema;
      end if;
    end;
  end if;
  if json_changed then
    if (num_nonnulls(new.request_observation_json #>> '{scheduling,capacityUsedSlots}', new.request_observation_json #>> '{scheduling,capacityTotalSlots}') in (0, 2)
        and num_nonnulls(new.request_observation_json #>> '{transport,connection,id}', new.request_observation_json #>> '{transport,connection,exitReason}',
          new.request_observation_json #>> '{transport,connection,ageMs}', new.request_observation_json #>> '{transport,connection,idleMs}') in (0, 4)) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_fact_completeness_ck',
          message = 'request observation violates model_requests_fact_completeness_ck', table = tg_table_name, schema = tg_table_schema;

    end if;
  end if;
  if json_changed or old.recovered_at is distinct from new.recovered_at or old.completed_at is distinct from new.completed_at then
    if (new.recovered_at is null or (new.completed_at is not null
        and new.request_observation_json #>> '{recovery,retryDelayMs}' is not null
        and new.request_observation_json #>> '{recovery,totalLatencyMs}' is not null)) is false then
        raise exception using errcode = '23514', constraint = 'model_requests_fact_completeness_ck',
          message = 'request observation violates model_requests_fact_completeness_ck', table = tg_table_name, schema = tg_table_schema;

    end if;
  end if;
  return null;
end;
$$;

drop trigger model_requests_observation_update on model_requests;
create trigger model_requests_observation_update
  after update on model_requests for each row
  when (old.request_observation_json is distinct from new.request_observation_json
    or old.completed_at is distinct from new.completed_at
    or old.continuation_requested is distinct from new.continuation_requested
    or old.recovered_at is distinct from new.recovered_at
    or old.recovery_attempt_count is distinct from new.recovery_attempt_count
    or old.recovery_request_id is distinct from new.recovery_request_id
) execute function check_model_request_observation();

alter table model_requests
  drop constraint model_requests_upstream_duration_ck,
  drop constraint model_requests_revision_attempt_ck,
  drop constraint model_requests_status_ck,
  drop constraint model_requests_websocket_pool_ck,
  drop constraint model_requests_routing_scope_ck,
  drop constraint model_requests_routing_group_names_ck,
  drop constraint model_requests_latency_ck,
  drop constraint model_requests_runtime_pressure_ck,
  drop constraint model_requests_continuation_hashes_ck,
  drop constraint model_requests_continuation_reason_ck,
  drop constraint model_requests_upstream_connection_ck,
  drop constraint model_requests_recovery_ck,
  drop constraint model_requests_fact_completeness_ck;

-- 不读取 JSON 的廉价约束保留为 CHECK，也保持与其他标量 CHECK 的错误优先级
alter table model_requests
  add constraint model_requests_revision_attempt_ck check (attempt_count >= 0),
  add constraint model_requests_status_ck check (
    (client_status_code is null or client_status_code between 100 and 599)
    and (upstream_status_code is null or upstream_status_code between 100 and 599)),
  add constraint model_requests_continuation_hashes_ck check (
    continuation_affinity_hash is null or continuation_affinity_hash ~ '^[0-9a-f]{64}$'),
  add constraint model_requests_recovery_ck check (recovery_attempt_count >= 0),
  add constraint model_requests_fact_completeness_ck check (
    (provider_account_id is null or provider_account_ref is not null)
    and (cost_source = 'unavailable' or cost_currency is not null));
