pragma foreign_keys = on;

-- ============================================
-- Admin Users
-- ============================================
create table admin_users (
  id text primary key,
  password_hash text not null,
  created_at text not null,
  updated_at text not null
);

-- ============================================
-- Admin Sessions
-- ============================================
create table admin_sessions (
  id text primary key,
  user_id text not null references admin_users(id) on delete cascade,
  expires_at text not null,
  created_at text not null
);

create index idx_admin_sessions_expires on admin_sessions(expires_at);

-- ============================================
-- Client API Keys
-- ============================================
create table client_api_keys (
  id text primary key,
  name text not null,
  prefix text not null,
  key text not null unique,
  label text,
  enabled integer not null default 1 check (enabled in (0, 1)),
  created_at text not null,
  last_used_at text
);

create index idx_client_api_keys_key_enabled on client_api_keys(key) where enabled = 1;
create index idx_client_api_keys_created_id on client_api_keys(created_at desc, id desc);

-- ============================================
-- Runtime Settings
-- ============================================
create table runtime_settings (
  id integer primary key check (id = 1),
  model_aliases_json text not null default '{}',
  refresh_margin_seconds integer not null check (refresh_margin_seconds > 0),
  refresh_concurrency integer not null check (refresh_concurrency > 0),
  max_concurrent_per_account integer not null check (max_concurrent_per_account > 0),
  request_interval_ms integer not null check (request_interval_ms >= 0),
  rotation_strategy text not null check (rotation_strategy in ('least_used', 'round_robin', 'sticky')),
  admin_api_key text,
  updated_at text not null
);

-- ============================================
-- Accounts
-- ============================================
create table accounts (
  id text primary key,
  email text,
  chatgpt_account_id text,
  chatgpt_user_id text,
  label text,
  plan_type text,
  access_token text not null,
  refresh_token text,
  access_token_expires_at text,
  next_refresh_at text,
  status text not null check (status in ('active', 'expired', 'quota_exhausted', 'refreshing', 'disabled', 'banned')),
  quota_json text,
  quota_fetched_at text,
  quota_limit_reached integer not null default 0 check (quota_limit_reached in (0, 1)),
  quota_verify_required integer not null default 0 check (quota_verify_required in (0, 1)),
  quota_cooldown_until text,
  cloudflare_cooldown_until text,
  added_at text not null,
  updated_at text not null
);

create index idx_accounts_status on accounts(status);
create index idx_accounts_added_id on accounts(added_at desc, id desc);
create unique index ux_accounts_chatgpt_identity
on accounts(chatgpt_account_id, coalesce(chatgpt_user_id, ''))
where chatgpt_account_id is not null;

-- ============================================
-- Account Refresh Leases
-- ============================================
create table account_refresh_leases (
  account_id text primary key references accounts(id) on delete cascade,
  owner text not null,
  expires_at text not null,
  updated_at text not null
);

create index idx_account_refresh_leases_expires on account_refresh_leases(expires_at);

-- ============================================
-- Account Usage Statistics
-- ============================================
create table account_usage (
  account_id text primary key references accounts(id) on delete cascade,
  request_count integer not null default 0 check (request_count >= 0),
  empty_response_count integer not null default 0 check (empty_response_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  reasoning_tokens integer not null default 0 check (reasoning_tokens >= 0),
  total_tokens integer not null default 0 check (total_tokens >= 0),
  image_input_tokens integer not null default 0 check (image_input_tokens >= 0),
  image_output_tokens integer not null default 0 check (image_output_tokens >= 0),
  image_request_count integer not null default 0 check (image_request_count >= 0),
  image_request_failed_count integer not null default 0 check (image_request_failed_count >= 0),
  window_request_count integer not null default 0 check (window_request_count >= 0),
  window_input_tokens integer not null default 0 check (window_input_tokens >= 0),
  window_output_tokens integer not null default 0 check (window_output_tokens >= 0),
  window_cached_tokens integer not null default 0 check (window_cached_tokens >= 0),
  window_image_input_tokens integer not null default 0 check (window_image_input_tokens >= 0),
  window_image_output_tokens integer not null default 0 check (window_image_output_tokens >= 0),
  window_image_request_count integer not null default 0 check (window_image_request_count >= 0),
  window_image_request_failed_count integer not null default 0 check (window_image_request_failed_count >= 0),
  window_started_at text,
  window_reset_at text,
  limit_window_seconds integer check (limit_window_seconds is null or limit_window_seconds > 0),
  last_used_at text
);

create index idx_account_usage_last_used_account
on account_usage(last_used_at desc, account_id desc);

-- ============================================
-- Account Model Usage Statistics
-- ============================================
create table account_model_usage (
  account_id text not null references accounts(id) on delete cascade,
  model text not null,
  request_count integer not null default 0 check (request_count >= 0),
  error_count integer not null default 0 check (error_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  last_used_at text,
  primary key (account_id, model)
);

create index idx_account_model_usage_last_used
on account_model_usage(last_used_at desc, account_id, model);

-- ============================================
-- Usage Time Buckets
-- ============================================
create table usage_time_buckets (
  bucket_start text not null,
  account_id text not null default '',
  model text not null default '',
  service_tier text not null default '',
  request_count integer not null default 0 check (request_count >= 0),
  error_count integer not null default 0 check (error_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  first_token_latency_sum integer not null default 0 check (first_token_latency_sum >= 0),
  first_token_latency_count integer not null default 0 check (first_token_latency_count >= 0),
  latency_sum integer not null default 0 check (latency_sum >= 0),
  latency_count integer not null default 0 check (latency_count >= 0),
  max_latency_ms integer not null default 0 check (max_latency_ms >= 0),
  min_latency_ms integer not null default 0 check (min_latency_ms >= 0),
  updated_at text not null,
  primary key (bucket_start, account_id, model, service_tier)
);

create index idx_usage_time_buckets_bucket
on usage_time_buckets(bucket_start);
create index idx_usage_time_buckets_model_bucket
on usage_time_buckets(model, bucket_start);

-- ============================================
-- Account Cookies
-- ============================================
create table account_cookies (
  id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  domain text not null,
  name text not null,
  value text not null,
  path text not null default '/',
  expires_at text,
  updated_at text not null,
  unique(account_id, domain, name, path)
);

create index idx_account_cookies_account on account_cookies(account_id);
create index idx_account_cookies_account_domain on account_cookies(account_id, domain);
create index idx_account_cookies_expires
on account_cookies(expires_at)
where expires_at is not null;

-- ============================================
-- Device Fingerprints
-- ============================================
create table fingerprints (
  id text primary key,
  originator text not null,
  app_version text not null,
  build_number text not null,
  platform text not null,
  arch text not null,
  chromium_version text not null,
  user_agent_template text not null,
  default_headers_json text not null,
  header_order_json text not null,
  source text not null,
  created_at text not null,
  updated_at text not null
);

create table fingerprint_update_history (
  id text primary key,
  current_fingerprint_id text not null references fingerprints(id) on delete cascade,
  app_version text not null,
  build_number text not null,
  chromium_version text,
  source text not null,
  manifest_json text,
  created_at text not null
);

create index idx_fingerprint_update_history_created_id
on fingerprint_update_history(created_at desc, id desc);

-- ============================================
-- Usage Records
-- ============================================
create table usage_records (
  id text primary key,
  request_id text,
  kind text not null,
  level text not null check (level in ('debug', 'info', 'warn', 'error')),
  account_id text,
  route text,
  model text,
  status_code integer check (status_code is null or (status_code >= 100 and status_code <= 599)),
  transport text,
  attempt_index integer check (attempt_index is null or attempt_index >= 0),
  upstream_status_code integer check (upstream_status_code is null or (upstream_status_code >= 100 and upstream_status_code <= 599)),
  failure_class text,
  response_id text,
  upstream_request_id text,
  latency_ms integer check (latency_ms is null or latency_ms >= 0),
  message text not null,
  metadata_json text not null,
  created_at text not null
);

create index idx_usage_records_created_id on usage_records(created_at desc, id desc);
create index idx_usage_records_kind_created on usage_records(kind, created_at desc);
create index idx_usage_records_request_id on usage_records(request_id);
create index idx_usage_records_account on usage_records(account_id, created_at desc) where account_id is not null;
create index idx_usage_records_transport on usage_records(transport, created_at desc) where transport is not null;
create index idx_usage_records_failure_class on usage_records(failure_class, created_at desc) where failure_class is not null;
create index idx_usage_records_response_id on usage_records(response_id) where response_id is not null;
create index idx_usage_records_upstream_request_id on usage_records(upstream_request_id) where upstream_request_id is not null;
create index idx_usage_records_level_created on usage_records(level, created_at desc);
create index idx_usage_records_route_created on usage_records(route, created_at desc) where route is not null;
create index idx_usage_records_model_created on usage_records(model, created_at desc) where model is not null;
create index idx_usage_records_status_created on usage_records(status_code, created_at desc) where status_code is not null;
create index idx_usage_records_upstream_status_created on usage_records(upstream_status_code, created_at desc) where upstream_status_code is not null;

-- ============================================
-- Model Plan Snapshots
-- ============================================
create table model_plan_snapshots (
  plan_type text primary key,
  models_json text not null,
  fetched_at text not null
);

-- ============================================
-- Session Affinities
-- ============================================
create table session_affinities (
  response_id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  conversation_id text not null,
  turn_state text,
  instructions_hash text,
  input_tokens integer check (input_tokens is null or input_tokens >= 0),
  function_call_ids_json text not null default '[]',
  variant_hash text,
  expires_at text not null,
  created_at text not null
);

create index idx_session_affinities_conversation on session_affinities(conversation_id, created_at desc);
create index idx_session_affinities_expires on session_affinities(expires_at);
create index idx_session_affinities_active_order on session_affinities(expires_at, created_at, response_id);
