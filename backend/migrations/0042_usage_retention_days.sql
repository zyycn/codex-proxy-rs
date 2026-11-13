-- 保留期仅要求正整数天数，沿用当前设置与默认值
alter table runtime_settings drop constraint runtime_settings_retention_ck;
alter table runtime_settings add constraint runtime_settings_retention_ck check (
    usage_retention_days > 0
    and ops_event_retention_days > 0
    and audit_retention_days > 0
);
