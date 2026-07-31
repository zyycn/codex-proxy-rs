-- Provider 账号观测快照：请求/事件发生时账号的名称、邮箱与认证类型。
-- 账号删除后 provider_account_id 外键置空，历史显示仍依赖这些快照列；
-- provider_account_ref 继续承担稳定筛选与关联事实。

alter table model_requests
  add column provider_account_name_snapshot text,
  add column provider_account_email_snapshot text,
  add column provider_account_authentication_kind_snapshot text;

alter table ops_events
  add column provider_account_name_snapshot text,
  add column provider_account_email_snapshot text,
  add column provider_account_authentication_kind_snapshot text;

-- 迁移时仍存在账号的历史行回填快照；已删除账号的历史行保持为空，
-- 读取侧回退 provider_account_ref。迁移前已删除账号的资料无法恢复。
update model_requests mr
set provider_account_name_snapshot = pa.name,
    provider_account_email_snapshot = pa.email,
    provider_account_authentication_kind_snapshot = pa.authentication_kind
from provider_accounts pa
where pa.id = mr.provider_account_ref;

update ops_events oe
set provider_account_name_snapshot = pa.name,
    provider_account_email_snapshot = pa.email,
    provider_account_authentication_kind_snapshot = pa.authentication_kind
from provider_accounts pa
where pa.id = oe.provider_account_ref;
