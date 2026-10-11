-- 历史引用与快照不依赖实体是否仍存在，切换前必须停止旧写入者
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
    ('{request}'::text[], array['clientApiKeyNameSnapshot','clientIp','compact','configRevision','endpoint','protocol','reasoningEffort','reasoningPreset','subagentKind','userAgent']::text[]),
    ('{account}'::text[], array['authenticationKind','email','name','planTypeSnapshot','notesSnapshot']::text[]),
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
    ('{request,clientApiKeyNameSnapshot}'::text[], 'string'),
    ('{account,planTypeSnapshot}'::text[], 'string'),
    ('{account,notesSnapshot}'::text[], 'string'),
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

drop view model_request_observations;
alter table model_requests
  drop constraint model_requests_client_fk,
  drop constraint model_requests_account_fk,
  drop constraint model_requests_client_ref_ck,
  drop constraint model_requests_account_ref_ck,
  drop constraint model_requests_fact_completeness_ck,
  drop column client_api_key_id,
  drop column provider_account_id,
  add constraint model_requests_fact_completeness_ck check (cost_source = 'unavailable' or cost_currency is not null),
  add constraint model_requests_client_ref_ck check (octet_length(client_api_key_ref) between 1 and 128),
  add constraint model_requests_account_ref_ck check (provider_account_ref is null or octet_length(provider_account_ref) between 1 and 128);
alter table ops_events
  drop constraint ops_events_account_fk,
  drop constraint ops_events_account_ref_ck,
  drop constraint ops_events_account_snapshot_present_ck,
  drop column provider_account_id,
  add column provider_account_plan_type_snapshot text,
  add constraint ops_events_account_ref_ck check (provider_account_ref is null or octet_length(provider_account_ref) between 1 and 128);

create or replace view model_request_observations as
select mr.*,
       (mr.request_observation_json #>> '{request,configRevision}')::bigint as config_revision,
       (mr.request_observation_json #>> '{request,protocol}') as protocol,
       (mr.request_observation_json #>> '{request,endpoint}') as endpoint,
       (mr.request_observation_json #>> '{account,name}') as provider_account_name_snapshot,
       (mr.request_observation_json #>> '{account,email}') as provider_account_email_snapshot,
       (mr.request_observation_json #>> '{account,authenticationKind}') as provider_account_authentication_kind_snapshot,
       (mr.request_observation_json #>> '{routing,scope}') as routing_scope,
       array(select jsonb_array_elements_text(mr.request_observation_json #> '{routing,groupRefs}')) as routing_group_refs,
       (mr.request_observation_json #> '{routing,groupNamesSnapshot}') as routing_group_names_snapshot,
       (mr.request_observation_json #>> '{error,providerErrorCode}') as provider_error_code,
       (mr.request_observation_json #>> '{error,message}') as error_message,
       (mr.request_observation_json #>> '{error,retryAfterMs}')::bigint as retry_after_ms,
       (mr.request_observation_json #>> '{request,clientIp}')::inet as client_ip,
       (mr.request_observation_json #>> '{request,userAgent}') as user_agent,
       (mr.request_observation_json #>> '{request,reasoningEffort}') as reasoning_effort,
       (mr.request_observation_json #>> '{request,reasoningPreset}') as reasoning_preset,
       (mr.request_observation_json #>> '{request,subagentKind}') as subagent_kind,
       (mr.request_observation_json #>> '{request,compact}')::boolean as compact,
       measured.transport_decision_wait_ms as transport_decision_wait_ms,
       measured.connect_ms as connect_ms,
       measured.headers_ms as headers_ms,
       measured.first_event_ms as first_event_ms,
       measured.first_reasoning_ms as first_reasoning_ms,
       measured.first_text_ms as first_text_ms,
       measured.first_token_ms as first_token_ms,
       measured.latency_ms as latency_ms,
       measured.provider_processing_ms as provider_processing_ms,
       measured.admission_decision_ms as admission_decision_ms,
       measured.account_selection_wait_ms as account_selection_wait_ms,
       measured.capacity_used_slots as capacity_used_slots,
       measured.capacity_total_slots as capacity_total_slots,
       (mr.request_observation_json #>> '{transport,httpVersion}') as http_version,
       (mr.request_observation_json #>> '{transport,websocketPool}') as websocket_pool,
       (mr.request_observation_json #>> '{transport,connection,id}') as upstream_connection_id,
       (mr.request_observation_json #>> '{transport,connection,exitReason}') as upstream_connection_exit_reason,
       (mr.request_observation_json #>> '{transport,connection,ageMs}')::bigint as upstream_connection_age_ms,
       (mr.request_observation_json #>> '{transport,connection,idleMs}')::bigint as upstream_connection_idle_ms,
       (mr.request_observation_json #>> '{response,responseModel}') as upstream_response_model,
       (mr.request_observation_json #>> '{continuation,previousResponseIdHash}') as continuation_previous_response_id_hash,
       (mr.request_observation_json #>> '{continuation,unavailableReason}') as continuation_unavailable_reason,
       (mr.request_observation_json #>> '{recovery,retryDelayMs}')::bigint as recovery_retry_delay_ms,
       (mr.request_observation_json #>> '{recovery,totalLatencyMs}')::bigint as recovery_total_latency_ms,
       (mr.request_observation_json #>> '{timings,upstream,apiOverheadMs}')::double precision as upstream_api_overhead_ms,
       (mr.request_observation_json #>> '{timings,upstream,engineMs}')::double precision as upstream_engine_ms,
       (mr.request_observation_json #>> '{timings,upstream,engineIapiTtftMs}')::double precision as upstream_engine_iapi_ttft_ms,
       (mr.request_observation_json #>> '{timings,upstream,engineServiceTtftMs}')::double precision as upstream_engine_service_ttft_ms,
       (mr.request_observation_json #>> '{timings,upstream,engineIapiTbtMs}')::double precision as upstream_engine_iapi_tbt_ms,
       (mr.request_observation_json #>> '{timings,upstream,engineServiceTbtMs}')::double precision as upstream_engine_service_tbt_ms,
       measured.upstream_response_ms as upstream_response_ms,
       mr.request_observation_json #>> '{request,clientApiKeyNameSnapshot}' as client_api_key_name_snapshot,
       mr.request_observation_json #>> '{account,planTypeSnapshot}' as provider_account_plan_type_snapshot,
       mr.request_observation_json #>> '{account,notesSnapshot}' as provider_account_notes_snapshot
from model_requests mr
cross join lateral (
  select (mr.request_observation_json #>> '{timings,local,transportDecisionWaitMs}')::bigint as transport_decision_wait_ms,
         (mr.request_observation_json #>> '{timings,local,connectMs}')::bigint as connect_ms,
         (mr.request_observation_json #>> '{timings,local,headersMs}')::bigint as headers_ms,
         (mr.request_observation_json #>> '{timings,local,firstEventMs}')::bigint as first_event_ms,
         (mr.request_observation_json #>> '{timings,local,firstReasoningMs}')::bigint as first_reasoning_ms,
         (mr.request_observation_json #>> '{timings,local,firstTextMs}')::bigint as first_text_ms,
         (mr.request_observation_json #>> '{timings,local,firstTokenMs}')::bigint as first_token_ms,
         (mr.request_observation_json #>> '{timings,local,latencyMs}')::bigint as latency_ms,
         (mr.request_observation_json #>> '{timings,upstream,processingMs}')::bigint as provider_processing_ms,
         (mr.request_observation_json #>> '{scheduling,admissionDecisionMs}')::bigint as admission_decision_ms,
         (mr.request_observation_json #>> '{scheduling,accountSelectionWaitMs}')::bigint as account_selection_wait_ms,
         (mr.request_observation_json #>> '{scheduling,capacityUsedSlots}')::bigint as capacity_used_slots,
         (mr.request_observation_json #>> '{scheduling,capacityTotalSlots}')::bigint as capacity_total_slots,
         (mr.request_observation_json #>> '{timings,upstream,responseMs}')::bigint as upstream_response_ms
) measured;
