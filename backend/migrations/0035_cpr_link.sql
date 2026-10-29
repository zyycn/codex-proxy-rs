-- 互联公开权限与本节点身份
alter table client_api_keys
  add column peer_sharing_json jsonb not null
  default '{"enabled":false,"sharePool":false,"shareQuota":false,"sharePlans":false}';

create table peer_node (
  singleton boolean primary key default true check (singleton),
  id uuid not null default gen_random_uuid(),
  name text not null default 'CPR 节点'
);
insert into peer_node (singleton) values (true);

-- 每个连接仅保留当前观测与可恢复计划，删除账号时一并清理
create table provider_account_upstream_info (
  account_id text primary key references provider_accounts(id) on delete cascade,
  credential_revision bigint not null check (credential_revision > 0),
  connection_revision text not null check (connection_revision ~ '^[a-f0-9]{64}$'),
  request_generation bigint not null default 1 check (request_generation > 0),
  completed_generation bigint not null default 0 check (completed_generation >= 0),
  next_attempt_at timestamptz not null,
  claimed_until timestamptz,
  claim_token uuid,
  last_attempt_at timestamptz,
  last_success_at timestamptz,
  last_error text check (last_error in ('unauthorized', 'unsupported', 'rate_limited', 'invalid_response', 'unavailable')),
  consecutive_failures integer not null default 0 check (consecutive_failures >= 0),
  snapshot_json jsonb check (jsonb_typeof(snapshot_json) = 'object'),
  constraint upstream_info_generation_ck check (completed_generation <= request_generation),
  constraint upstream_info_claim_ck check ((claim_token is null) = (claimed_until is null))
);
create index provider_account_upstream_info_due_idx
  on provider_account_upstream_info (next_attempt_at, account_id);
