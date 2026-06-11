pragma foreign_keys = on;

create table if not exists admin_users (
  id text primary key,
  password_hash text not null,
  created_at text not null,
  updated_at text not null
);

create table if not exists admin_sessions (
  id text primary key,
  user_id text not null references admin_users(id) on delete cascade,
  expires_at text not null,
  created_at text not null
);

create table if not exists client_api_keys (
  id text primary key,
  name text not null,
  prefix text not null,
  key_hash text not null,
  enabled integer not null default 1,
  created_at text not null,
  last_used_at text
);

create table if not exists accounts (
  id text primary key,
  email text,
  account_id text,
  user_id text,
  label text,
  plan_type text,
  access_token_cipher text not null,
  refresh_token_cipher text,
  status text not null,
  quota_json text,
  quota_fetched_at text,
  added_at text not null,
  updated_at text not null
);

create table if not exists account_usage (
  account_id text primary key references accounts(id) on delete cascade,
  request_count integer not null default 0,
  input_tokens integer not null default 0,
  output_tokens integer not null default 0,
  cached_tokens integer not null default 0,
  last_used_at text
);

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

create table if not exists event_logs (
  id text primary key,
  request_id text,
  kind text not null,
  level text not null,
  account_id text,
  route text,
  model text,
  status_code integer,
  latency_ms integer,
  message text not null,
  metadata_json text not null,
  created_at text not null
);
