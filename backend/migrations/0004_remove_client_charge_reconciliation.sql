-- 移除人工核账：历史未取得费用的请求按零结算，已记录金额保持不变。
-- 迁移在监听请求前执行；遗留未完成记录不再阻断 Key。
update client_key_charge_events
set state = 'settled',
    amount_usd = coalesce(amount_usd, 0),
    completed_at = coalesce(completed_at, least(deadline_at, now()));

-- 费用表只保存已完成的幂等结算，不再维护预扣费和待核账状态。
drop index client_key_charge_events_unresolved;
alter table client_key_charge_events
    drop column state,
    drop column started_at,
    drop column deadline_at,
    drop column reconciled_at,
    drop column reconciliation_reason,
    alter column amount_usd set not null,
    alter column completed_at set not null;
