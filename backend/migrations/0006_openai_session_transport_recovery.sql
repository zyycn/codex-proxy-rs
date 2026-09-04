create index model_requests_pending_ws_transport_recovery_idx
  on model_requests (
    client_api_key_ref,
    continuation_affinity_hash,
    completed_at desc,
    id desc
  )
  where provider_kind = 'openai'
    and outcome = 'failed'
    and error_kind = 'upstream_unavailable'
    and upstream_transport = 'websocket'
    and upstream_send_state = 'ambiguous'
    and downstream_committed_at is null
    and recovered_at is null
    and continuation_affinity_hash is not null;

with session_transport_recoveries as (
  select
    prior.id as failed_request_id,
    recovered.id as recovery_request_id,
    recovered.started_at as recovery_started_at,
    recovered.completed_at as recovered_at
  from model_requests prior
  cross join lateral (
    select current.id, current.started_at, current.completed_at
    from model_requests current
    where current.client_api_key_ref = prior.client_api_key_ref
      and current.continuation_affinity_hash = prior.continuation_affinity_hash
      and current.provider_kind = prior.provider_kind
      and current.outcome = 'succeeded'
      and current.upstream_transport = 'http_sse'
      and current.completed_at is not null
      and current.started_at >= prior.completed_at
      and current.started_at <= prior.completed_at + interval '30 seconds'
    order by current.started_at, current.id
    limit 1
  ) recovered
  where prior.provider_kind = 'openai'
    and prior.outcome = 'failed'
    and prior.error_kind = 'upstream_unavailable'
    and prior.upstream_transport = 'websocket'
    and prior.upstream_send_state = 'ambiguous'
    and prior.downstream_committed_at is null
    and prior.recovered_at is null
    and prior.continuation_affinity_hash is not null
)
update model_requests prior
set recovery_attempt_count = prior.recovery_attempt_count + 1,
    recovery_request_id = recovery.recovery_request_id,
    recovered_at = recovery.recovered_at,
    recovery_retry_delay_ms = greatest(
      0,
      floor(extract(epoch from (recovery.recovery_started_at - prior.completed_at)) * 1000)
    )::bigint,
    recovery_total_latency_ms = greatest(
      0,
      floor(extract(epoch from (recovery.recovered_at - prior.completed_at)) * 1000)
    )::bigint
from session_transport_recoveries recovery
where prior.id = recovery.failed_request_id
  and prior.recovered_at is null;
