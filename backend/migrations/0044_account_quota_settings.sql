-- 将已有窗口上限保留为额度桶设置，预热默认关闭
alter table provider_accounts rename column quota_limits to quota_settings;
update provider_accounts a set quota_settings = (
    select coalesce(jsonb_object_agg(key, jsonb_build_object('limitPercent', value, 'warmup', null)), '{}'::jsonb)
    from jsonb_each(a.quota_settings)
);

-- 执行进度与管理配置分开，后台写入不发布配置版本
alter table provider_accounts
    add column quota_warmup_state jsonb not null default '{}'::jsonb,
    add column quota_warmup_revision bigint not null default 0;
