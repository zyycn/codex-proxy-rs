-- 额度导出仅保留启用账号，不再配置或传递调度状态
alter table client_api_keys drop column quota_sharing_include_status;

-- 已保存的展示快照移除停用条目及旧调度字段，保留其余额度和模型数据
update provider_account_upstream_quota_snapshots
set payload = jsonb_set(payload, '{accounts}', coalesce((
    select jsonb_agg(entry - 'enabled' order by ordinal)
    from jsonb_array_elements(payload->'accounts') with ordinality as account(entry, ordinal)
    where entry->'enabled' is distinct from 'false'::jsonb
), '[]'::jsonb))
where payload is not null;
