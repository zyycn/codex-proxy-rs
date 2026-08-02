-- 移除账号级 cooldown 状态模型（照 v2 语义：429 限流不进 availability，由 quota 数据驱动）。
--
-- 存量迁移：
--   - availability='cooldown' 的账号：429/Cloudflare 等临时冷却 → 'ready'
--     （限流事实留在 provider_quota_json，由投影滚动兜底）；
--     402 语义的 usage_limit_exhausted 且未到期 → 'quota_exhausted'。

-- 1. 迁移存量 cooldown 账号。
update provider_accounts
set availability = case
      when availability_reason = 'usage_limit_exhausted'
           and cooldown_until is not null
           and cooldown_until > now()
        then 'quota_exhausted'
      else 'ready'
    end
where availability = 'cooldown';

-- 2. 删除依赖 cooldown_until 的约束与索引。
alter table provider_accounts drop constraint provider_accounts_cooldown_ck;
drop index provider_accounts_availability_idx;

-- 3. availability CHECK 去掉 'cooldown'。
alter table provider_accounts drop constraint provider_accounts_availability_ck;
alter table provider_accounts
  add constraint provider_accounts_availability_ck check (
    availability in (
      'unknown',
      'ready',
      'quota_exhausted',
      'expired',
      'banned',
      'invalid'
    )
  );

-- 4. 删除列。
alter table provider_accounts drop column availability_reason;
alter table provider_accounts drop column cooldown_until;

-- 5. 重建可用性索引（不含 cooldown_until）。
create index provider_accounts_availability_idx
  on provider_accounts (availability, id);
