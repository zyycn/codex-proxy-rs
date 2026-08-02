-- 修正 0003 存量回填：raw JSON 缺 rate_limit 键时表达式为 NULL，
-- 赋 NULL 违反 not-null 约束。0004 把仍为 NULL 的行兜底为 false
-- （0003 已应用的环境，add column 用 default false 填充，通常无 NULL；
-- 本迁移保证任何环境下该列都非 NULL）。

update provider_accounts
set quota_limit_reached = coalesce(quota_limit_reached, false)
where quota_limit_reached is null;
