pragma foreign_keys = on;

-- ============================================
-- Admin Users
-- ============================================
create table if not exists admin_users (
  id text primary key,
  password_hash text not null,
  created_at text not null,
  updated_at text not null
);

-- ============================================
-- Admin Sessions
-- ============================================
create table if not exists admin_sessions (
  id text primary key,
  user_id text not null references admin_users(id) on delete cascade,
  expires_at text not null,
  created_at text not null
);

create index if not exists idx_admin_sessions_expires on admin_sessions(expires_at);

-- ============================================
-- Client API Keys
-- ============================================
create table if not exists client_api_keys (
  id text primary key,
  name text not null,
  prefix text not null,
  key_hash text not null,
  label text,
  enabled integer not null default 1 check (enabled in (0, 1)),
  created_at text not null,
  last_used_at text
);

create index if not exists idx_client_api_keys_prefix on client_api_keys(prefix) where enabled = 1;

-- ============================================
-- Accounts
-- ============================================
create table if not exists accounts (
  id text primary key,
  email text,
  account_id text,
  user_id text,
  label text,
  plan_type text,
  access_token_cipher text not null,
  refresh_token_cipher text,
  access_token_expires_at text,
  status text not null check (status in ('active', 'expired', 'quota_exhausted', 'refreshing', 'disabled', 'banned')),
  quota_json text,
  quota_fetched_at text,
  quota_limit_reached integer not null default 0 check (quota_limit_reached in (0, 1)),
  quota_cooldown_until text,
  cloudflare_cooldown_until text,
  added_at text not null,
  updated_at text not null
);

create index if not exists idx_accounts_status on accounts(status);
create index if not exists idx_accounts_identity on accounts(account_id, user_id) where account_id is not null;

-- ============================================
-- Account Refresh Leases
-- ============================================
create table if not exists account_refresh_leases (
  account_id text primary key references accounts(id) on delete cascade,
  owner text not null,
  expires_at text not null,
  updated_at text not null
);

create index if not exists idx_account_refresh_leases_expires on account_refresh_leases(expires_at);

-- ============================================
-- Account Usage Statistics
-- ============================================
create table if not exists account_usage (
  account_id text primary key references accounts(id) on delete cascade,
  request_count integer not null default 0 check (request_count >= 0),
  empty_response_count integer not null default 0 check (empty_response_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  window_request_count integer not null default 0 check (window_request_count >= 0),
  window_input_tokens integer not null default 0 check (window_input_tokens >= 0),
  window_output_tokens integer not null default 0 check (window_output_tokens >= 0),
  window_cached_tokens integer not null default 0 check (window_cached_tokens >= 0),
  window_started_at text,
  window_reset_at text,
  limit_window_seconds integer check (limit_window_seconds is null or limit_window_seconds > 0),
  last_used_at text
);

-- ============================================
-- Account Cookies
-- ============================================
create table if not exists account_cookies (
  id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  domain text not null,
  name text not null,
  value_cipher text not null,
  path text not null default '/',
  expires_at text,
  updated_at text not null,
  unique(account_id, domain, name, path)
);

create index if not exists idx_account_cookies_account on account_cookies(account_id);
create index if not exists idx_account_cookies_account_domain on account_cookies(account_id, domain);

-- ============================================
-- Device Fingerprints
-- ============================================
create table if not exists fingerprints (
  id text primary key,
  app_version text not null,
  build_number text not null,
  platform text not null,
  arch text not null,
  chromium_version text not null,
  user_agent_template text not null,
  source text not null,
  created_at text not null
);

-- ============================================
-- Event Logs
-- ============================================
create table if not exists event_logs (
  id text primary key,
  request_id text,
  kind text not null,
  level text not null check (level in ('debug', 'info', 'warn', 'error')),
  account_id text,
  route text,
  model text,
  status_code integer check (status_code is null or (status_code >= 100 and status_code <= 599)),
  latency_ms integer check (latency_ms is null or latency_ms >= 0),
  message text not null,
  metadata_json text not null,
  created_at text not null
);

create index if not exists idx_event_logs_created_id on event_logs(created_at desc, id desc);
create index if not exists idx_event_logs_kind_created on event_logs(kind, created_at desc);
create index if not exists idx_event_logs_request_id on event_logs(request_id);
create index if not exists idx_event_logs_account on event_logs(account_id, created_at desc) where account_id is not null;

-- ============================================
-- Model Plan Snapshots
-- ============================================
create table if not exists model_plan_snapshots (
  plan_type text primary key,
  models_json text not null,
  fetched_at text not null
);

-- ============================================
-- Session Affinities
-- ============================================
create table if not exists session_affinities (
  response_id text primary key,
  account_id text not null,
  conversation_id text not null,
  turn_state text,
  instructions_hash text,
  input_tokens integer check (input_tokens is null or input_tokens >= 0),
  function_call_ids_json text not null default '[]',
  variant_hash text,
  expires_at text not null,
  created_at text not null
);

create index if not exists idx_session_affinities_conversation on session_affinities(conversation_id, created_at desc);
create index if not exists idx_session_affinities_expires on session_affinities(expires_at);
