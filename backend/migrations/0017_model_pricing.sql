-- 覆盖配置与请求历史明细分离，改价不重算已发生的费用。
alter table runtime_settings add column pricing_overrides_json jsonb not null default '{}'
    check (jsonb_typeof(pricing_overrides_json) = 'object');
alter table runtime_settings add column pricing_synced_json jsonb not null default '{}'
    check (jsonb_typeof(pricing_synced_json) = 'object');
alter table runtime_settings add column pricing_synced_at timestamptz;
alter table model_requests add column billing_snapshot_json jsonb
    check (billing_snapshot_json is null or jsonb_typeof(billing_snapshot_json) = 'object');
