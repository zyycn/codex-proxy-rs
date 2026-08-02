-- 物化快照级限流事实：Dashboard 复用 Provider 解析结果，不再 SQL 解析 raw quota JSON。
--
-- `quota_limit_reached` 由 Provider 在写 quota 时计算（快照级：顶层或任一窗口触顶，
-- 已过 reset 的窗口不计数），Admin/Dashboard 直接读该列。

alter table provider_accounts
  add column quota_limit_reached boolean not null default false;

-- 存量回填：从现有 raw JSON 推导（仅 primary_window 的 limit_reached + reset 未过期）。
update provider_accounts
set quota_limit_reached = coalesce(
  provider_quota_json is not null
  and provider_quota_json->'rate_limit'->>'limit_reached' = 'true'
  and (
    provider_quota_json->'rate_limit'->'primary_window'->>'reset_at' is null
    or (provider_quota_json->'rate_limit'->'primary_window'->>'reset_at')::bigint > extract(epoch from now())::bigint
  ),
  false
)
where provider_quota_json is not null;
