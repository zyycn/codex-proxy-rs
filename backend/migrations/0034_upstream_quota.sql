-- Key 的额度披露策略独立于推理预算，历史 Key 默认不披露账号池
alter table client_api_keys add column quota_sharing_enabled boolean not null default false;
alter table client_api_keys add column quota_identity_secret text not null default gen_random_uuid()::text;

-- 展示快照不进入原生 quota 状态机，连接版本隔离旧上游的晚到响应
create table provider_account_upstream_quota_snapshots (
    account_id text primary key references provider_accounts(id) on delete cascade,
    credential_revision bigint not null,
    attempt_id text,
    last_attempt_at timestamptz,
    fetched_at timestamptz,
    last_error_code text,
    forwarding_blocked boolean not null default false,
    payload jsonb
);
