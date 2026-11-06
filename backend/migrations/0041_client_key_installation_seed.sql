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
