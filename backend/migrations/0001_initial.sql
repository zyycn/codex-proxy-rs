-- 管理员身份与密码摘要。
create table admin_users (
  id text primary key,
  password_hash text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  constraint admin_users_time_ck check (created_at <= updated_at)
);

-- 管理操作审计事件。
create table admin_audit_events (
  id text primary key,
  actor_kind text not null,
  actor_admin_user_id text,
  actor_ref text not null,
  admin_request_id text,
  action text not null,
  entity_kind text not null,
  entity_ref text not null,
  config_revision bigint,
  changed_fields text[] not null default '{}',
  created_at timestamptz not null,
  constraint admin_audit_events_actor_kind_ck check (
    actor_kind in ('admin_session', 'admin_api_key', 'system', 'anonymous')
  ),
  constraint admin_audit_events_config_revision_ck check (
    config_revision is null or config_revision > 0
  ),
  constraint admin_audit_events_changed_fields_ck check (
    cardinality(changed_fields) <= 64
    and array_position(changed_fields, null) is null
  ),
  constraint admin_audit_events_actor_fk foreign key (actor_admin_user_id)
    references admin_users (id)
    on update restrict
    on delete set null
);

create index admin_audit_events_actor_idx
  on admin_audit_events (actor_admin_user_id, created_at desc, id desc)
  where actor_admin_user_id is not null;
create index admin_audit_events_created_idx
  on admin_audit_events (created_at desc, id desc);
create index admin_audit_events_entity_idx
  on admin_audit_events (entity_kind, entity_ref, created_at desc, id desc);
create index admin_audit_events_actor_ref_idx
  on admin_audit_events (actor_ref, created_at desc, id desc);

-- 客户端 API 密钥、鉴权范围与限额。
create table client_api_keys (
  id text primary key,
  name text not null,
  label text,
  key text not null unique,
  enabled boolean not null default true,
  max_concurrency bigint not null default 0,
  requests_per_minute bigint not null default 0,
  last_used_at timestamptz,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  constraint client_api_keys_key_ck check (
    key ~ '^sk_[A-Za-z0-9_-]{43}$'
  ),
  constraint client_api_keys_limits_ck check (
    max_concurrency >= 0
    and requests_per_minute >= 0
  ),
  constraint client_api_keys_time_ck check (created_at <= updated_at)
);

create index client_api_keys_created_idx
  on client_api_keys (created_at desc, id desc);
create index client_api_keys_last_used_idx
  on client_api_keys (last_used_at desc, id desc);

-- 全局运行参数与配置版本。
create table runtime_settings (
  id bigint primary key,
  config_revision bigint not null default 1,
  admin_api_key text,
  refresh_margin_seconds bigint not null default 3600,
  refresh_concurrency bigint not null default 2,
  max_concurrent_per_account bigint not null default 3,
  request_interval_ms bigint not null default 50,
  rotation_strategy text not null default 'smart',
  model_mappings_json jsonb not null default '{}'::jsonb,
  usage_retention_days bigint not null default 31,
  ops_event_retention_days bigint not null default 30,
  audit_retention_days bigint not null default 90,
  updated_at timestamptz not null,
  constraint runtime_settings_singleton_ck check (id = 1),
  constraint runtime_settings_revision_ck check (config_revision > 0),
  constraint runtime_settings_refresh_ck check (
    refresh_margin_seconds > 0
    and refresh_concurrency > 0
    and max_concurrent_per_account > 0
    and request_interval_ms >= 0
  ),
  constraint runtime_settings_rotation_ck check (
    rotation_strategy in ('smart', 'quota_reset_priority', 'round_robin', 'sticky')
  ),
  constraint runtime_settings_model_mappings_ck check (
    jsonb_typeof(model_mappings_json) = 'object'
    and octet_length(model_mappings_json::text) <= 131072
  ),
  constraint runtime_settings_retention_ck check (
    usage_retention_days >= 31
    and ops_event_retention_days > 0
    and audit_retention_days > 0
  )
);

-- 上游 Provider 账号、凭据与可用性状态。
create table provider_accounts (
  id text primary key,
  provider_kind text not null,
  name text not null,
  email text,
  upstream_user_id text,
  upstream_account_id text,
  plan_type text,
  authentication_kind text not null,
  provider_credentials_json jsonb not null,
  credential_revision bigint not null default 1,
  has_refresh_token boolean not null,
  access_token_expires_at timestamptz,
  next_refresh_at timestamptz,
  enabled boolean not null default true,
  credential_state text not null default 'unknown',
  provider_quota_json jsonb,
  last_error_message text,
  credential_observed_at timestamptz not null,
  quota_observed_at timestamptz,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  quota_access_state text not null default 'unknown',
  quota_evidence text,
  quota_access_observed_at timestamptz,
  quota_reset_at timestamptz,
  last_error_reason text,
  concurrency_limit bigint,
  weight smallint not null default 1,
  constraint provider_accounts_revision_ck check (credential_revision > 0),
  constraint provider_accounts_authentication_kind_ck check (
    authentication_kind ~ '^[a-z][a-z0-9_]{0,63}$'
  ),
  constraint provider_accounts_credentials_ck check (
    jsonb_typeof(provider_credentials_json) = 'object'
    and octet_length(provider_credentials_json::text) <= 262144
  ),
  constraint provider_accounts_quota_ck check (
    provider_quota_json is null
    or (
      jsonb_typeof(provider_quota_json) = 'object'
      and octet_length(provider_quota_json::text) <= 131072
    )
  ),
  constraint provider_accounts_refresh_ck check (
    has_refresh_token or next_refresh_at is null
  ),
  constraint provider_accounts_credential_state_ck check (
    credential_state in ('unknown', 'ready', 'expired', 'banned', 'invalid')
  ),
  constraint provider_accounts_quota_access_state_ck check (
    quota_access_state in ('unknown', 'allowed', 'exhausted')
  ),
  constraint provider_accounts_quota_evidence_ck check (
    quota_evidence is null
    or quota_evidence in (
      'provider_denied',
      'account_limit_reached',
      'usage_limit_reached',
      'payment_required'
    )
  ),
  constraint provider_accounts_quota_fact_ck check (
    (quota_access_state = 'unknown' and quota_evidence is null and quota_reset_at is null)
    or (
      quota_access_state = 'allowed'
      and quota_evidence is null
      and quota_access_observed_at is not null
      and quota_reset_at is null
    )
    or (
      quota_access_state = 'exhausted'
      and quota_evidence is not null
      and quota_access_observed_at is not null
    )
  ),
  constraint provider_accounts_quota_observation_ck check (
    (provider_quota_json is null) = (quota_observed_at is null)
  ),
  constraint provider_accounts_error_reason_ck check (
    last_error_reason is null
    or last_error_reason in (
      'account_unverified',
      'access_token_expired',
      'credential_expired',
      'credential_invalid',
      'account_banned'
    )
  ),
  constraint provider_accounts_concurrency_limit_ck check (
    concurrency_limit is null or concurrency_limit between 1 and 4294967295
  ),
  constraint provider_accounts_weight_ck check (
    weight between 1 and 100
  ),
  constraint provider_accounts_time_ck check (
    created_at <= updated_at
    and credential_observed_at <= updated_at
    and (quota_access_observed_at is null or quota_access_observed_at <= updated_at)
    and (quota_observed_at is null or quota_observed_at <= updated_at)
  )
);

create unique index provider_accounts_upstream_identity_uq
  on provider_accounts (
    provider_kind,
    upstream_user_id,
    coalesce(upstream_account_id, '')
  );
create index provider_accounts_runtime_idx
  on provider_accounts (provider_kind, enabled, id);
create index provider_accounts_credential_state_idx
  on provider_accounts (credential_state, id);
create index provider_accounts_quota_access_idx
  on provider_accounts (quota_access_state, quota_reset_at, id);
create index provider_accounts_access_expiry_idx
  on provider_accounts (access_token_expires_at, id);
create index provider_accounts_refresh_due_idx
  on provider_accounts (next_refresh_at, id)
  where enabled and has_refresh_token and next_refresh_at is not null;
create index provider_accounts_email_idx
  on provider_accounts (provider_kind, lower(email))
  where email is not null;

-- 与 Provider 无关的账号组，以及账号和客户端 API 密钥的组成员关系。
create table account_groups (
  id text primary key,
  name text not null,
  description text,
  enabled boolean not null default true,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  color text not null,
  constraint account_groups_id_ck check (
    id ~ '^grp_[0-9a-f]{32}$'
  ),
  constraint account_groups_name_ck check (
    char_length(btrim(name)) between 1 and 100
    and name = btrim(name)
    and name !~ '[[:cntrl:]]'
  ),
  constraint account_groups_description_ck check (
    description is null or (
      octet_length(description) <= 4096
      and description !~ '[[:cntrl:]]'
    )
  ),
  constraint account_groups_time_ck check (
    created_at <= updated_at
  ),
  constraint account_groups_color_ck check (color ~ '^#[0-9A-F]{8}$')
);

create unique index account_groups_name_uq
  on account_groups (lower(name));
create index account_groups_list_idx
  on account_groups (enabled, created_at desc, id desc);

create table account_group_accounts (
  account_group_id text not null,
  provider_account_id text not null,
  created_at timestamptz not null,
  primary key (account_group_id, provider_account_id),
  constraint account_group_accounts_group_fk foreign key (account_group_id)
    references account_groups (id)
    on update restrict
    on delete cascade,
  constraint account_group_accounts_account_fk foreign key (provider_account_id)
    references provider_accounts (id)
    on update restrict
    on delete cascade
);

create index account_group_accounts_account_idx
  on account_group_accounts (provider_account_id, account_group_id);

create table client_api_key_groups (
  client_api_key_id text not null,
  account_group_id text not null,
  created_at timestamptz not null,
  primary key (client_api_key_id, account_group_id),
  constraint client_api_key_groups_key_fk foreign key (client_api_key_id)
    references client_api_keys (id)
    on update restrict
    on delete cascade,
  constraint client_api_key_groups_group_fk foreign key (account_group_id)
    references account_groups (id)
    on update restrict
    on delete restrict
);

create index client_api_key_groups_group_idx
  on client_api_key_groups (account_group_id, client_api_key_id);

-- 模型请求、上游尝试、用量、计费与结果。
create table model_requests (
  id text primary key,
  client_api_key_id text,
  client_api_key_ref text not null,
  config_revision bigint not null,
  protocol text not null,
  operation text not null,
  endpoint text not null,
  client_transport text not null,
  requested_model_id text,
  provider_kind text,
  upstream_model_id text,
  service_tier text,
  provider_account_id text,
  provider_account_ref text,
  provider_account_name_snapshot text,
  provider_account_email_snapshot text,
  provider_account_authentication_kind_snapshot text,
  upstream_transport text,
  http_version text,
  websocket_pool text,
  provider_observation_json jsonb,
  attempt_count integer not null default 0,
  upstream_send_state text not null default 'not_sent',
  downstream_committed_at timestamptz,
  outcome text not null default 'running',
  client_status_code integer,
  upstream_status_code integer,
  client_response_id bytea,
  upstream_request_id text,
  upstream_response_id bytea,
  error_kind text,
  provider_error_code text,
  error_message text,
  retry_after_ms bigint,
  input_tokens bigint,
  output_tokens bigint,
  cached_tokens bigint,
  cache_write_tokens bigint,
  reasoning_tokens bigint,
  image_input_tokens bigint,
  image_output_tokens bigint,
  total_tokens bigint,
  image_generation_succeeded boolean,
  cost_source text not null default 'unavailable',
  cost_amount numeric(20, 10),
  cost_currency text,
  transport_decision_wait_ms bigint,
  connect_ms bigint,
  headers_ms bigint,
  first_event_ms bigint,
  first_reasoning_ms bigint,
  first_text_ms bigint,
  first_token_ms bigint,
  provider_processing_ms bigint,
  latency_ms bigint,
  client_ip inet,
  user_agent text,
  reasoning_effort text,
  reasoning_preset text,
  request_kind text,
  subagent_kind text,
  compact boolean not null default false,
  image_generation_requested boolean not null default false,
  started_at timestamptz not null,
  deadline_at timestamptz not null,
  completed_at timestamptz,
  routing_scope text not null,
  routing_group_refs text[] not null default '{}',
  routing_group_names_snapshot jsonb not null default '[]'::jsonb,
  constraint model_requests_client_ref_ck check (
    client_api_key_id is null or client_api_key_id = client_api_key_ref
  ),
  constraint model_requests_account_ref_ck check (
    provider_account_id is null or provider_account_id = provider_account_ref
  ),
  constraint model_requests_revision_attempt_ck check (
    config_revision > 0 and attempt_count >= 0
  ),
  constraint model_requests_send_state_ck check (
    upstream_send_state in ('not_sent', 'sent', 'ambiguous')
    and (
      (
        attempt_count = 0
        and upstream_send_state = 'not_sent'
      )
      or (
        attempt_count > 0
        and provider_kind is not null
        and provider_account_ref is not null
        and upstream_transport is not null
      )
    )
  ),
  constraint model_requests_outcome_ck check (
    outcome in ('running', 'succeeded', 'failed', 'cancelled', 'incomplete')
  ),
  constraint model_requests_status_ck check (
    (client_status_code is null or client_status_code between 100 and 599)
    and (upstream_status_code is null or upstream_status_code between 100 and 599)
    and (retry_after_ms is null or retry_after_ms >= 0)
  ),
  constraint model_requests_tokens_ck check (
    (input_tokens is null or input_tokens >= 0)
    and (output_tokens is null or output_tokens >= 0)
    and (cached_tokens is null or cached_tokens >= 0)
    and (cache_write_tokens is null or cache_write_tokens >= 0)
    and (reasoning_tokens is null or reasoning_tokens >= 0)
    and (image_input_tokens is null or image_input_tokens >= 0)
    and (image_output_tokens is null or image_output_tokens >= 0)
    and (total_tokens is null or total_tokens >= 0)
  ),
  constraint model_requests_image_generation_ck check (
    (
      not image_generation_requested
      and image_generation_succeeded is null
    )
    or (
      image_generation_requested
      and (
        (outcome = 'running' and image_generation_succeeded is null)
        or (outcome <> 'running' and image_generation_succeeded is not null)
      )
    )
  ),
  constraint model_requests_websocket_pool_ck check (
    websocket_pool is null or websocket_pool in ('new', 'reuse')
  ),
  constraint model_requests_provider_observation_ck check (
    provider_observation_json is null
    or jsonb_typeof(provider_observation_json) = 'object'
  ),
  constraint model_requests_service_tier_ck check (
    service_tier is null
    or (
      octet_length(service_tier) between 1 and 64
      and service_tier !~ '[[:cntrl:]]'
    )
  ),
  constraint model_requests_routing_scope_ck check (
    routing_scope in ('legacy_provider', 'all', 'groups')
  ),
  constraint model_requests_routing_group_names_ck check (
    jsonb_typeof(routing_group_names_snapshot) = 'array'
    and (
      (
        routing_scope in ('legacy_provider', 'all')
        and cardinality(routing_group_refs) = 0
        and jsonb_array_length(routing_group_names_snapshot) = 0
      )
      or (
        routing_scope = 'groups'
        and cardinality(routing_group_refs) > 0
        and array_position(routing_group_refs, null) is null
        and jsonb_array_length(routing_group_names_snapshot)
          = cardinality(routing_group_refs)
      )
    )
  ),
  constraint model_requests_cost_ck check (
    cost_source in ('provider_reported', 'calculated', 'unavailable')
    and (
      (
        cost_source = 'unavailable'
        and cost_amount is null
        and cost_currency is null
      )
      or (
        cost_source in ('provider_reported', 'calculated')
        and cost_amount is not null
        and cost_amount >= 0
        and cost_currency ~ '^[A-Z]{3}$'
      )
    )
  ),
  constraint model_requests_latency_ck check (
    (transport_decision_wait_ms is null or transport_decision_wait_ms >= 0)
    and (connect_ms is null or connect_ms >= 0)
    and (headers_ms is null or headers_ms >= 0)
    and (first_event_ms is null or first_event_ms >= 0)
    and (first_reasoning_ms is null or first_reasoning_ms >= 0)
    and (first_text_ms is null or first_text_ms >= 0)
    and (first_token_ms is null or first_token_ms >= 0)
    and (provider_processing_ms is null or provider_processing_ms >= 0)
    and (latency_ms is null or latency_ms >= 0)
    and (
      latency_ms is null
      or (
        (transport_decision_wait_ms is null or transport_decision_wait_ms <= latency_ms)
        and (connect_ms is null or connect_ms <= latency_ms)
        and (headers_ms is null or headers_ms <= latency_ms)
        and (first_event_ms is null or first_event_ms <= latency_ms)
        and (first_reasoning_ms is null or first_reasoning_ms <= latency_ms)
        and (first_text_ms is null or first_text_ms <= latency_ms)
        and (first_token_ms is null or first_token_ms <= latency_ms)
        and (provider_processing_ms is null or provider_processing_ms <= latency_ms)
      )
    )
  ),
  constraint model_requests_lifecycle_ck check (
    started_at <= deadline_at
    and (
      (outcome = 'running' and completed_at is null)
      or (
        outcome <> 'running'
        and completed_at is not null
        and completed_at >= started_at
      )
    )
  ),
  constraint model_requests_client_fk foreign key (client_api_key_id)
    references client_api_keys (id)
    on update restrict
    on delete set null,
  constraint model_requests_account_fk foreign key (provider_account_id)
    references provider_accounts (id)
    on update restrict
    on delete set null
);

create index model_requests_client_idx
  on model_requests (client_api_key_id, started_at desc, id desc);
create index model_requests_account_idx
  on model_requests (
    provider_account_id,
    provider_kind,
    started_at desc,
    id desc
  );
create index model_requests_started_idx
  on model_requests (started_at desc, id desc);
create index model_requests_client_ref_idx
  on model_requests (client_api_key_ref, started_at desc, id desc);
create index model_requests_account_ref_idx
  on model_requests (provider_account_ref, started_at desc, id desc)
  where provider_account_ref is not null;
create index model_requests_requested_model_idx
  on model_requests (requested_model_id, started_at desc, id desc);
create index model_requests_upstream_model_idx
  on model_requests (upstream_model_id, started_at desc, id desc)
  where upstream_model_id is not null;
create index model_requests_provider_idx
  on model_requests (provider_kind, started_at desc, id desc)
  where provider_kind is not null;
create index model_requests_outcome_idx
  on model_requests (outcome, started_at desc, id desc);
create index model_requests_running_deadline_idx
  on model_requests (deadline_at, id)
  where outcome = 'running';
create index model_requests_retention_idx
  on model_requests (completed_at)
  where outcome <> 'running';
create index model_requests_routing_group_refs_idx
  on model_requests using gin (routing_group_refs);

-- 运行期警告与错误事件。
create table ops_events (
  id text primary key,
  model_request_id text,
  attempt_index integer,
  level text not null,
  component text not null,
  operation text not null,
  provider_kind text,
  provider_account_id text,
  provider_account_ref text,
  provider_account_name_snapshot text,
  provider_account_email_snapshot text,
  provider_account_authentication_kind_snapshot text,
  upstream_model_id text,
  failure_kind text not null,
  status_code integer,
  provider_error_code text,
  retry_after_ms bigint,
  upstream_request_id text,
  latency_ms bigint,
  message text not null,
  occurrence_count integer not null default 1,
  created_at timestamptz not null,
  constraint ops_events_request_attempt_ck check (
    (
      model_request_id is null
      and attempt_index is null
    )
    or (
      model_request_id is not null
      and attempt_index is not null
      and attempt_index > 0
    )
  ),
  constraint ops_events_level_ck check (level in ('warning', 'error')),
  constraint ops_events_values_ck check (
    (status_code is null or status_code between 100 and 599)
    and (retry_after_ms is null or retry_after_ms >= 0)
    and (latency_ms is null or latency_ms >= 0)
    and occurrence_count > 0
  ),
  constraint ops_events_account_ref_ck check (
    provider_account_id is null or provider_account_id = provider_account_ref
  ),
  constraint ops_events_request_fk foreign key (model_request_id)
    references model_requests (id)
    on update restrict
    on delete cascade,
  constraint ops_events_account_fk foreign key (provider_account_id)
    references provider_accounts (id)
    on update restrict
    on delete set null
);

create index ops_events_request_idx
  on ops_events (model_request_id, attempt_index, id);
create index ops_events_account_idx
  on ops_events (
    provider_account_id,
    provider_kind,
    created_at desc,
    id desc
  );
create index ops_events_created_idx
  on ops_events (created_at desc, id desc);
create index ops_events_component_idx
  on ops_events (component, created_at desc, id desc);
create index ops_events_failure_idx
  on ops_events (failure_kind, created_at desc, id desc);
create index ops_events_account_ref_idx
  on ops_events (provider_account_ref, created_at desc, id desc)
  where provider_account_ref is not null;
create index ops_events_retention_idx
  on ops_events (created_at)
  where model_request_id is null;

insert into runtime_settings (id, updated_at)
values (1, now());

-- S3 兼容对象存储、备份计划与保留策略。
create table backup_settings (
  id bigint primary key,
  storage_revision bigint not null default 1,
  endpoint text,
  region text,
  bucket text,
  access_key_id text,
  secret_access_key text,
  prefix text,
  force_path_style boolean not null default false,
  schedule_enabled boolean not null default false,
  cron_expression text,
  schedule_timezone text default 'Asia/Shanghai',
  retention_days bigint not null default 0,
  retention_count bigint not null default 0,
  next_run_at timestamptz,
  last_verified_at timestamptz,
  updated_at timestamptz not null,
  constraint backup_settings_singleton_ck check (id = 1),
  constraint backup_settings_revision_ck check (storage_revision > 0),
  constraint backup_settings_endpoint_ck check (
    endpoint is null
    or (
      endpoint ~ '^https?://'
      and endpoint !~ '[[:space:][:cntrl:]]'
      and endpoint !~ '://[^/]*@'
    )
  ),
  constraint backup_settings_lengths_ck check (
    (bucket is null or (octet_length(bucket) between 1 and 255 and bucket !~ '[[:cntrl:]]'))
    and (region is null or (octet_length(region) between 1 and 64 and region !~ '[[:cntrl:]]'))
    and (prefix is null or (octet_length(prefix) <= 1024 and prefix !~ '[[:cntrl:]]'))
    and (
      access_key_id is null
      or (octet_length(access_key_id) between 1 and 255 and access_key_id !~ '[[:cntrl:]]')
    )
    and (
      secret_access_key is null
      or (
        octet_length(secret_access_key) between 1 and 512
        and secret_access_key !~ '[[:cntrl:]]'
      )
    )
  ),
  constraint backup_settings_schedule_ck check (
    (cron_expression is null or cron_expression ~ '^[^[:space:]]+([[:space:]]+[^[:space:]]+){4}$')
    and (
      schedule_timezone is null
      or (octet_length(schedule_timezone) between 1 and 64 and schedule_timezone !~ '[[:cntrl:]]')
    )
    and retention_days >= 0
    and retention_count >= 0
    and (
      not schedule_enabled
      or (
        endpoint is not null
        and bucket is not null
        and region is not null
        and access_key_id is not null
        and secret_access_key is not null
        and prefix is not null
        and cron_expression is not null
        and schedule_timezone is not null
        and last_verified_at is not null
      )
    )
  ),
  -- 计划启停与游标一致性：启用必有 next_run_at，关闭必清空。
  constraint backup_settings_cursor_ck check (
    (not schedule_enabled and next_run_at is null)
    or (schedule_enabled and next_run_at is not null)
  )
);

insert into backup_settings (
  id,
  storage_revision,
  schedule_enabled,
  retention_days,
  retention_count,
  force_path_style,
  schedule_timezone,
  updated_at
)
values (1, 1, false, 0, 0, false, 'Asia/Shanghai', now());

-- 备份任务状态与归档信息。
create table backup_records (
  id text primary key,
  trigger_kind text not null,
  status text not null,
  scheduled_at timestamptz,
  object_key text not null,
  size_bytes bigint,
  sha256 text,
  expires_at timestamptz,
  attempt_count integer not null default 0,
  error_code text,
  error_message text,
  started_at timestamptz,
  completed_at timestamptz,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  constraint backup_records_id_ck check (id ~ '^backup_[0-9a-f]{32}$'),
  constraint backup_records_trigger_kind_ck check (
    trigger_kind in ('manual', 'scheduled')
    and (
      (trigger_kind = 'manual' and scheduled_at is null)
      or (trigger_kind = 'scheduled' and scheduled_at is not null)
    )
  ),
  constraint backup_records_status_ck check (
    status in ('queued', 'dumping', 'uploading', 'completed', 'failed', 'deleting')
  ),
  constraint backup_records_attempt_ck check (attempt_count >= 0),
  constraint backup_records_lifecycle_ck check (
    created_at <= updated_at
    and (started_at is null or started_at >= created_at)
    and (completed_at is null or completed_at >= created_at)
    and (
      (
        status = 'queued'
        and started_at is null
        and completed_at is null
      )
      or (
        status in ('dumping', 'uploading')
        and started_at is not null
        and completed_at is null
      )
      or (
        status in ('completed', 'failed', 'deleting')
        and started_at is not null
        and completed_at is not null
      )
    )
  ),
  constraint backup_records_artifact_ck check (
    (
      status not in ('uploading', 'completed')
      or (size_bytes is not null and sha256 is not null)
    )
    and (size_bytes is null or size_bytes >= 0)
    and (sha256 is null or sha256 ~ '^[0-9a-f]{64}$')
  ),
  constraint backup_records_error_ck check (
    (error_code is null) = (error_message is null)
    and (status <> 'failed' or (error_code is not null and error_message is not null))
    and (
      status not in ('queued', 'dumping', 'uploading', 'completed')
      or error_code is null
    )
  ),
  -- expires_at 为空，或不早于创建时间（创建时 now + 天数 恒晚于 created_at）。
  constraint backup_records_expiry_ck check (
    expires_at is null or expires_at >= created_at
  )
);

create index backup_records_created_idx
  on backup_records (created_at desc, id desc);
create index backup_records_status_idx
  on backup_records (status, created_at, id);
create unique index backup_records_scheduled_uq
  on backup_records (scheduled_at)
  where trigger_kind = 'scheduled' and scheduled_at is not null;
create unique index backup_records_active_uq
  on backup_records ((1))
  where status in ('queued', 'dumping', 'uploading');
create unique index backup_records_object_key_uq
  on backup_records (object_key)
  where object_key is not null;
create index backup_records_scheduled_completed_idx
  on backup_records (completed_at desc, id desc)
  where trigger_kind = 'scheduled' and status = 'completed';
create index backup_records_retention_idx
  on backup_records (expires_at)
  where expires_at is not null and status in ('completed', 'failed');
