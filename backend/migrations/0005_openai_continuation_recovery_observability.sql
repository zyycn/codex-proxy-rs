alter table model_requests
  add column continuation_affinity_hash text,
  add column continuation_previous_response_id_hash text,
  add column continuation_requested boolean not null default false,
  add column continuation_unavailable_reason text,
  add column upstream_connection_id text,
  add column upstream_connection_exit_reason text,
  add column upstream_connection_age_ms bigint,
  add column upstream_connection_idle_ms bigint,
  add column recovery_request_id text,
  add column recovered_at timestamptz,
  add column recovery_attempt_count integer not null default 0,
  add column recovery_retry_delay_ms bigint,
  add column recovery_total_latency_ms bigint,
  add constraint model_requests_continuation_hashes_ck check (
    (continuation_affinity_hash is null
      or continuation_affinity_hash ~ '^[0-9a-f]{64}$')
    and (continuation_previous_response_id_hash is null
      or continuation_previous_response_id_hash ~ '^[0-9a-f]{64}$')
    and (continuation_requested
      or continuation_previous_response_id_hash is null)
  ),
  add constraint model_requests_continuation_reason_ck check (
    continuation_unavailable_reason is null
    or (
      octet_length(continuation_unavailable_reason) between 1 and 64
      and continuation_unavailable_reason ~ '^[a-z][a-z0-9_]*$'
    )
  ),
  add constraint model_requests_upstream_connection_ck check (
    (
      upstream_connection_id is null
      and upstream_connection_exit_reason is null
      and upstream_connection_age_ms is null
      and upstream_connection_idle_ms is null
    )
    or (
      octet_length(upstream_connection_id) between 1 and 128
      and upstream_connection_id !~ '[[:cntrl:]]'
      and octet_length(upstream_connection_exit_reason) between 1 and 64
      and upstream_connection_exit_reason ~ '^[a-z][a-z0-9_]*$'
      and upstream_connection_age_ms >= 0
      and upstream_connection_idle_ms between 0 and upstream_connection_age_ms
    )
  ),
  add constraint model_requests_recovery_ck check (
    recovery_attempt_count >= 0
    and (
      (
        recovered_at is null
        and recovery_request_id is null
        and recovery_retry_delay_ms is null
        and recovery_total_latency_ms is null
      )
      or (
        recovered_at is not null
        and recovered_at >= completed_at
        and recovery_request_id is not null
        and octet_length(recovery_request_id) between 1 and 128
        and recovery_attempt_count > 0
        and recovery_retry_delay_ms >= 0
        and recovery_total_latency_ms >= recovery_retry_delay_ms
      )
    )
  );

create index model_requests_pending_continuation_recovery_idx
  on model_requests (
    client_api_key_ref,
    continuation_affinity_hash,
    completed_at desc,
    id desc
  )
  where error_kind = 'continuation_recovery_required'
    and recovered_at is null
    and continuation_affinity_hash is not null;

create index model_requests_recovery_request_idx
  on model_requests (recovery_request_id)
  where recovery_request_id is not null;
