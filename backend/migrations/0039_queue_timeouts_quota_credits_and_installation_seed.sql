-- 两层排队分别沿用实例已配置的时限，账号队满或超时默认返回错误
ALTER TABLE runtime_settings
    RENAME COLUMN concurrency_wait_timeout_seconds TO key_queue_timeout_seconds;

ALTER TABLE runtime_settings
    ADD COLUMN account_queue_timeout_seconds BIGINT NOT NULL DEFAULT 30
        CHECK (account_queue_timeout_seconds BETWEEN 1 AND 120),
    ADD COLUMN switch_account_on_queue_failure BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE runtime_settings SET account_queue_timeout_seconds = key_queue_timeout_seconds;

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

-- Key 安装身份只保存种子，现有安装 ID 转为种子后按 Key 与账号重新派生
WITH migrated AS (
    UPDATE client_api_keys
    SET provider_request_profiles_json = jsonb_set(
        provider_request_profiles_json,
        '{openai}',
        CASE
            WHEN provider_request_profiles_json #> '{openai,inherit}' ? 'installationId'
            THEN jsonb_set(
                provider_request_profiles_json -> 'openai',
                '{inherit}',
                ((provider_request_profiles_json #> '{openai,inherit}') - 'installationId')
                || jsonb_build_object(
                    'installationSeed',
                    provider_request_profiles_json #> '{openai,inherit,installationId}'
                )
            )
            ELSE ((provider_request_profiles_json -> 'openai') - 'installationId')
                || jsonb_build_object(
                    'installationSeed',
                    provider_request_profiles_json #> '{openai,installationId}'
                )
        END
    ),
    updated_at = GREATEST(now(), updated_at)
    WHERE provider_request_profiles_json -> 'openai' ? 'installationId'
       OR provider_request_profiles_json #> '{openai,inherit}' ? 'installationId'
    RETURNING id
)
UPDATE runtime_settings
SET config_revision = config_revision + 1,
    updated_at = GREATEST(now(), updated_at)
WHERE EXISTS (SELECT 1 FROM migrated);
