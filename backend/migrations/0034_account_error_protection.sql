-- 新安装与升级均从关闭且无规则开始，不继承旧保护配置
alter table runtime_settings add column account_error_protection_json jsonb not null default '{"enabled":false,"rules":[],"threshold":12,"windowSeconds":600,"durationSeconds":7200,"probeEnabled":true,"probeModel":null}'::jsonb;
alter table runtime_settings
    drop column account_auto_freeze_enabled,
    drop column account_auto_freeze_threshold,
    drop column account_auto_freeze_window_seconds,
    drop column account_auto_freeze_duration_seconds,
    drop column account_auto_freeze_probe_enabled,
    drop column account_auto_freeze_probe_model,
    drop column account_auto_freeze_adaptive_concurrency;

alter table runtime_settings add column account_error_protection_revision bigint not null default 1 check (account_error_protection_revision > 0);
update runtime_settings set account_error_protection_revision = config_revision;
