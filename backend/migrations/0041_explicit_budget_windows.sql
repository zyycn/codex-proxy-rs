-- 显式窗口复用 Key 费用账本，仅保留当前版本及最近一次窗口变更
alter table client_api_key_budget_windows
    add column daily_explicit boolean not null default false,
    add column weekly_explicit boolean not null default false,
    add column revision bigint not null default 0 check (revision >= 0),
    add column last_operation_request jsonb,
    add column charge_history_since timestamptz,
    add constraint daily_explicit_window_order check (not daily_explicit or daily_start < daily_end),
    add constraint weekly_explicit_window_order check (not weekly_explicit or weekly_start < weekly_end);

-- 窗口接管只扫描当前 Key 的周期内费用
create index client_api_key_charge_events_completed
    on client_api_key_charge_events (client_api_key_id, completed_at) include (amount_usd);

-- 旧业务备份恢复时会清空整张费用事件表，最早留存费用之前的历史无法确认
-- 在此之后创建的 Key 不可能有更早的消费
with coverage as (
    select coalesce(min(completed_at), now()) as since from client_api_key_charge_events
)
update client_api_key_budget_windows w set charge_history_since = coverage.since
from coverage, client_api_keys k
where k.id = w.client_api_key_id and k.created_at < coverage.since;
