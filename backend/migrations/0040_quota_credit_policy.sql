alter table provider_accounts
  add column auto_use_quota_credits boolean not null default false,
  drop constraint provider_accounts_quota_evidence_ck,
  drop constraint provider_accounts_quota_fact_ck,
  add constraint provider_accounts_quota_evidence_ck check (
    quota_evidence in ('provider_denied', 'account_limit_reached', 'usage_limit_reached', 'payment_required', 'credits_available')
  ),
  add constraint provider_accounts_quota_fact_ck check (
    (quota_access_state = 'unknown' and quota_evidence is null and quota_reset_at is null)
    or (quota_access_state = 'allowed' and quota_access_observed_at is not null
      and ((quota_evidence is null and quota_reset_at is null) or quota_evidence is not distinct from 'credits_available'))
    or (quota_access_state = 'exhausted' and quota_evidence is not null and quota_evidence <> 'credits_available' and quota_access_observed_at is not null)
  );

-- 在行锁内按当前配置投影仅点数可用的观测，晚返回的查询不能覆盖关闭开关的决定
create function apply_quota_credit_policy() returns trigger language plpgsql as $$
begin
  new.quota_access_state := 'exhausted';
  new.quota_evidence := 'account_limit_reached';
  return new;
end
$$;

create trigger provider_accounts_quota_credit_policy
before insert or update of auto_use_quota_credits, quota_evidence, quota_access_state on provider_accounts
for each row when (not new.auto_use_quota_credits and new.quota_evidence = 'credits_available') execute function apply_quota_credit_policy();
