-- 每个账号只保留最后一次成功观测，凭据轮换后旧目录不能成为新凭据的权限证据
create table provider_account_model_catalogs (
    account_id text primary key references provider_accounts(id) on delete cascade,
    credential_revision bigint not null check (credential_revision > 0),
    plan_type text,
    etag text,
    observed_at timestamptz not null,
    document bytea not null,
    document_hash bytea generated always as (sha256(document)) stored
);
