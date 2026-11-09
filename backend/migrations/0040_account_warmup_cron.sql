-- 清空旧时间计划并关闭预热，重新启用时由管理员填写 Cron
update runtime_settings set account_warmup_enabled = false;

alter table runtime_settings
    drop column account_warmup_schedule_time,
    add column account_warmup_cron text not null default '' check (
        octet_length(account_warmup_cron) <= 255
        and account_warmup_cron = btrim(account_warmup_cron)
        and account_warmup_cron !~ '[[:cntrl:]]'
        and (not account_warmup_enabled or account_warmup_cron <> '')
    );
