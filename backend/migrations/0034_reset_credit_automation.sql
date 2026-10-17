-- 主动重置卡配置与计划随账号保存，到期索引只覆盖开启自动使用的账号
ALTER TABLE runtime_settings
    ADD COLUMN reset_credit_lead_minutes integer NOT NULL DEFAULT 10
        CHECK (reset_credit_lead_minutes BETWEEN 1 AND 1440);

ALTER TABLE provider_accounts
    ADD COLUMN reset_credit_automation_json jsonb,
    ADD COLUMN reset_credit_next_run_at timestamptz,
    ADD CONSTRAINT provider_accounts_reset_credit_automation_shape CHECK (
        (reset_credit_automation_json IS NULL AND reset_credit_next_run_at IS NULL)
        OR coalesce((
            reset_credit_automation_json IS NOT NULL AND reset_credit_next_run_at IS NOT NULL
            AND jsonb_typeof(reset_credit_automation_json) = 'object'
            AND jsonb_typeof(reset_credit_automation_json -> 'enabled') = 'boolean'
            AND jsonb_typeof(reset_credit_automation_json -> 'revision') = 'number'
            AND jsonb_typeof(reset_credit_automation_json -> 'state') = 'object'
        ), false)
    );

CREATE INDEX provider_accounts_reset_credit_due_idx
    ON provider_accounts (reset_credit_next_run_at, id)
    WHERE enabled AND reset_credit_automation_json ->> 'enabled' = 'true';
