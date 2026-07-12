create table admin_users (
  id text primary key,
  password_hash text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
);

create table client_api_keys (
  id text primary key,
  name text not null,
  prefix text not null,
  key text not null unique,
  label text,
  enabled boolean not null default true,
  created_at timestamptz not null,
  last_used_at timestamptz
);

create index idx_client_api_keys_created_id
  on client_api_keys(created_at desc, id desc);

create table runtime_settings (
  id bigint primary key check (id = 1),
  model_aliases_json jsonb not null default '{}',
  refresh_margin_seconds bigint not null check (refresh_margin_seconds > 0),
  refresh_concurrency bigint not null check (refresh_concurrency > 0),
  max_concurrent_per_account bigint not null check (max_concurrent_per_account > 0),
  request_interval_ms bigint not null check (request_interval_ms >= 0),
  rotation_strategy text not null check (
    rotation_strategy in ('smart', 'quota_reset_priority', 'round_robin', 'sticky')
  ),
  admin_api_key_hash text,
  usage_retention_days bigint not null default 30 check (usage_retention_days > 0),
  ops_error_retention_days bigint not null default 30 check (ops_error_retention_days > 0),
  bucket_retention_days bigint not null default 90 check (bucket_retention_days > 0),
  updated_at timestamptz not null
);

create table accounts (
  id text primary key,
  email text,
  chatgpt_account_id text,
  chatgpt_user_id text,
  label text,
  plan_type text,
  access_token text not null,
  refresh_token text,
  access_token_expires_at timestamptz,
  next_refresh_at timestamptz,
  status text not null check (
    status in ('active', 'expired', 'quota_exhausted', 'disabled', 'banned')
  ),
  quota_json jsonb,
  quota_fetched_at timestamptz,
  quota_limit_reached boolean not null default false,
  quota_verify_required boolean not null default false,
  quota_cooldown_until timestamptz,
  cloudflare_cooldown_until timestamptz,
  added_at timestamptz not null,
  updated_at timestamptz not null
);

create index idx_accounts_added_id on accounts(added_at desc, id desc);
create unique index ux_accounts_chatgpt_identity
  on accounts(chatgpt_account_id, coalesce(chatgpt_user_id, ''))
  where chatgpt_account_id is not null;

create table account_usage (
  account_id text primary key references accounts(id) on delete cascade,
  request_count bigint not null default 0 check (request_count >= 0),
  empty_response_count bigint not null default 0 check (empty_response_count >= 0),
  input_tokens bigint not null default 0 check (input_tokens >= 0),
  output_tokens bigint not null default 0 check (output_tokens >= 0),
  cached_tokens bigint not null default 0 check (cached_tokens >= 0),
  reasoning_tokens bigint not null default 0 check (reasoning_tokens >= 0),
  total_tokens bigint not null default 0 check (total_tokens >= 0),
  image_input_tokens bigint not null default 0 check (image_input_tokens >= 0),
  image_output_tokens bigint not null default 0 check (image_output_tokens >= 0),
  image_request_count bigint not null default 0 check (image_request_count >= 0),
  image_request_failed_count bigint not null default 0 check (image_request_failed_count >= 0),
  window_request_count bigint not null default 0 check (window_request_count >= 0),
  window_input_tokens bigint not null default 0 check (window_input_tokens >= 0),
  window_output_tokens bigint not null default 0 check (window_output_tokens >= 0),
  window_cached_tokens bigint not null default 0 check (window_cached_tokens >= 0),
  window_image_input_tokens bigint not null default 0 check (window_image_input_tokens >= 0),
  window_image_output_tokens bigint not null default 0 check (window_image_output_tokens >= 0),
  window_image_request_count bigint not null default 0 check (window_image_request_count >= 0),
  window_image_request_failed_count bigint not null default 0 check (
    window_image_request_failed_count >= 0
  ),
  window_started_at timestamptz,
  window_reset_at timestamptz,
  limit_window_seconds bigint check (
    limit_window_seconds is null or limit_window_seconds > 0
  ),
  last_used_at timestamptz
);

create table usage_records (
  id text primary key,
  request_id text,
  client_api_key_id text,
  kind text not null,
  route text,
  provider text not null,
  account_id text not null,
  model text not null,
  requested_model text,
  upstream_model text,
  service_tier text,
  status_code integer not null check (status_code between 200 and 399),
  transport text,
  attempt_index bigint check (attempt_index is null or attempt_index >= 0),
  response_id text,
  upstream_request_id text,
  latency_ms bigint check (latency_ms is null or latency_ms >= 0),
  first_token_ms bigint check (first_token_ms is null or first_token_ms >= 0),
  input_tokens bigint check (input_tokens is null or input_tokens >= 0),
  output_tokens bigint check (output_tokens is null or output_tokens >= 0),
  cached_tokens bigint check (cached_tokens is null or cached_tokens >= 0),
  reasoning_tokens bigint check (reasoning_tokens is null or reasoning_tokens >= 0),
  message text not null,
  metadata_json jsonb not null,
  created_at timestamptz not null
);

create index idx_usage_records_created_id on usage_records(created_at desc, id desc);
create index idx_usage_records_request_id
  on usage_records(request_id) where request_id is not null;
create index idx_usage_records_kind_created
  on usage_records(kind, created_at desc);
create index idx_usage_records_account_created
  on usage_records(account_id, created_at desc);
create index idx_usage_records_model_created
  on usage_records(model, created_at desc);
create index idx_usage_records_key_created
  on usage_records(client_api_key_id, created_at desc)
  where client_api_key_id is not null;
create index idx_usage_records_response_id
  on usage_records(response_id) where response_id is not null;
create index idx_usage_records_upstream_request_id
  on usage_records(upstream_request_id) where upstream_request_id is not null;

create table ops_error_logs (
  id text primary key,
  request_id text,
  client_api_key_id text,
  kind text not null,
  provider text,
  account_id text,
  route text,
  model text,
  status_code integer check (
    status_code is null or status_code between 100 and 599
  ),
  client_status_code integer check (
    client_status_code is null or client_status_code between 100 and 599
  ),
  upstream_status_code integer check (
    upstream_status_code is null or upstream_status_code between 100 and 599
  ),
  transport text,
  attempt_index bigint check (attempt_index is null or attempt_index >= 0),
  failure_class text,
  response_id text,
  upstream_request_id text,
  latency_ms bigint check (latency_ms is null or latency_ms >= 0),
  message text not null,
  metadata_json jsonb not null,
  created_at timestamptz not null
);

create index idx_ops_error_logs_created_id
  on ops_error_logs(created_at desc, id desc);
create index idx_ops_error_logs_request_id
  on ops_error_logs(request_id) where request_id is not null;
create index idx_ops_error_logs_key_created
  on ops_error_logs(client_api_key_id, created_at desc)
  where client_api_key_id is not null;
create index idx_ops_error_logs_account
  on ops_error_logs(account_id, created_at desc) where account_id is not null;
create index idx_ops_error_logs_route_created
  on ops_error_logs(route, created_at desc) where route is not null;
create index idx_ops_error_logs_model_created
  on ops_error_logs(model, created_at desc) where model is not null;
create index idx_ops_error_logs_status_created
  on ops_error_logs(status_code, created_at desc) where status_code is not null;
create index idx_ops_error_logs_transport_created
  on ops_error_logs(transport, created_at desc) where transport is not null;
create index idx_ops_error_logs_failure_class
  on ops_error_logs(failure_class, created_at desc) where failure_class is not null;
create index idx_ops_error_logs_response_id
  on ops_error_logs(response_id) where response_id is not null;
create index idx_ops_error_logs_upstream_request_id
  on ops_error_logs(upstream_request_id) where upstream_request_id is not null;

create table request_time_buckets (
  bucket_start timestamptz not null,
  provider text not null default '__unknown__',
  account_id text not null default '__unknown__',
  model text not null default '__unknown__',
  service_tier text not null default '__unknown__',
  success_count bigint not null default 0 check (success_count >= 0),
  error_count bigint not null default 0 check (error_count >= 0),
  input_tokens bigint not null default 0 check (input_tokens >= 0),
  output_tokens bigint not null default 0 check (output_tokens >= 0),
  cached_tokens bigint not null default 0 check (cached_tokens >= 0),
  first_token_latency_sum bigint not null default 0 check (first_token_latency_sum >= 0),
  first_token_latency_count bigint not null default 0 check (first_token_latency_count >= 0),
  latency_sum bigint not null default 0 check (latency_sum >= 0),
  latency_count bigint not null default 0 check (latency_count >= 0),
  max_latency_ms bigint not null default 0 check (max_latency_ms >= 0),
  min_latency_ms bigint check (min_latency_ms is null or min_latency_ms >= 0),
  updated_at timestamptz not null,
  primary key (bucket_start, provider, account_id, model, service_tier)
);

create table account_cookies (
  id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  domain text not null,
  name text not null,
  value text not null,
  path text not null default '/',
  expires_at timestamptz,
  updated_at timestamptz not null,
  unique(account_id, domain, name, path)
);

create index idx_account_cookies_account_domain
  on account_cookies(account_id, domain);
create index idx_account_cookies_expires
  on account_cookies(expires_at) where expires_at is not null;

create table fingerprints (
  id text primary key,
  originator text not null,
  app_version text not null,
  build_number text not null,
  platform text not null,
  arch text not null,
  chromium_version text not null,
  user_agent_template text not null,
  default_headers_json jsonb not null,
  header_order_json jsonb not null,
  source text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
);

create table fingerprint_update_history (
  id text primary key,
  current_fingerprint_id text not null references fingerprints(id) on delete cascade,
  app_version text not null,
  build_number text not null,
  chromium_version text,
  source text not null,
  manifest_json jsonb,
  created_at timestamptz not null
);

create index idx_fingerprint_update_history_created_id
  on fingerprint_update_history(created_at desc, id desc);
create index idx_fingerprint_update_history_fingerprint
  on fingerprint_update_history(current_fingerprint_id);
