-- 租约只保存运行态，历史表不承担高频续期更新
-- 迁移在停止旧写入者的窗口执行，旧二进制不可直接回退
create table model_request_leases (
  request_id text primary key references model_requests(id) on delete cascade,
  lease_expires_at timestamptz not null
);
create index model_request_leases_expiry_idx on model_request_leases (lease_expires_at, request_id);
insert into model_request_leases (request_id, lease_expires_at)
select id, deadline_at from model_requests where outcome = 'running';
drop view model_request_observations;
drop index model_requests_running_deadline_idx;
alter table model_requests rename column deadline_at to recovery_lease_expires_at;
alter table model_requests alter column recovery_lease_expires_at drop not null;
update model_requests set recovery_lease_expires_at = null where outcome = 'running';

-- 主请求锁先于租约锁，终结和恢复共用同一顺序；续期只取得租约锁
create function finish_model_request_lease() returns trigger
language plpgsql as $$
declare
  expires_at timestamptz;
begin
  if old.outcome = 'running' and new.outcome <> 'running' then
    select lease_expires_at into expires_at from model_request_leases
      where request_id = new.id for update;
    new.recovery_lease_expires_at := coalesce(expires_at, old.recovery_lease_expires_at, old.started_at);
    delete from model_request_leases where request_id = new.id;
  end if;
  return new;
end;
$$;
create trigger model_requests_finish_lease before update on model_requests
  for each row when (old.outcome = 'running' and new.outcome <> 'running')
  execute function finish_model_request_lease();

create or replace view model_request_observations as
select mr.*,
       coalesce(lease.lease_expires_at, mr.recovery_lease_expires_at) as effective_recovery_lease_expires_at,
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
left join model_request_leases lease on lease.request_id = mr.id
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
