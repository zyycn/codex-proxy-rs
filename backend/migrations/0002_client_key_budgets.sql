alter table client_api_keys
    add column daily_limit_usd numeric(20,10) not null default 0 check (daily_limit_usd >= 0),
    add column weekly_limit_usd numeric(20,10) not null default 0 check (weekly_limit_usd >= 0);

create table client_key_budget_windows (
    client_api_key_id text primary key references client_api_keys(id) on delete cascade,
    daily_start timestamptz not null,
    daily_end timestamptz not null,
    weekly_start timestamptz not null,
    weekly_end timestamptz not null,
    daily_used_usd numeric(20,10) not null default 0 check (daily_used_usd >= 0),
    weekly_used_usd numeric(20,10) not null default 0 check (weekly_used_usd >= 0)
);

-- Deliberately independent of model_requests: observations may be dropped or pruned.
create table client_key_charge_events (
    request_id text primary key,
    client_api_key_id text not null references client_api_keys(id) on delete cascade,
    started_at timestamptz not null default now(),
    deadline_at timestamptz not null,
    completed_at timestamptz,
    amount_usd numeric(20,10) check (amount_usd >= 0),
    reconciled_at timestamptz,
    reconciliation_reason text,
    state text not null default 'pending' check (state in ('pending', 'unknown', 'settled')),
    check ((state = 'settled') = (amount_usd is not null))
);
create index client_key_charge_events_unresolved
    on client_key_charge_events(client_api_key_id, deadline_at)
    where state <> 'settled';
