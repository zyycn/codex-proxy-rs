-- 上游明确指出耗尽窗口时，随该次拒绝保留其时长
alter table provider_accounts
    add column quota_limit_window_minutes integer
    check (quota_limit_window_minutes between 1 and 65535);

-- 记录最近一次运行设置写入对应的全局发布版本，供编辑草稿判断冲突
alter table runtime_settings add column settings_revision bigint not null default 1;
update runtime_settings set settings_revision = config_revision;
alter table runtime_settings add constraint runtime_settings_edit_revision_ck
    check (settings_revision between 1 and config_revision);

-- 亲和配置由所有 Provider 共用，重命名保留已保存的模式与时长
alter table runtime_settings rename column openai_account_affinity to account_affinity;
alter table runtime_settings rename column openai_session_affinity_ttl_hours to session_affinity_ttl_hours;
alter table runtime_settings rename constraint runtime_settings_openai_account_affinity_check to runtime_settings_account_affinity_check;
alter table runtime_settings rename constraint runtime_settings_openai_session_affinity_ttl_hours_check to runtime_settings_session_affinity_ttl_hours_check;
