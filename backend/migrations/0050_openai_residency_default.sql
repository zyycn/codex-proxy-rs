alter table runtime_settings
    alter column openai_request_settings_json set default '{"requestCountry":{"mode":"follow_location"},"oauthGuardianReviewModel":true,"residency":"us","accountRoutingOverride":null}'::jsonb;

-- 升级时将空值初始化为 us，显式配置的区域保持原值
update runtime_settings
set openai_request_settings_json = jsonb_set(openai_request_settings_json, '{residency}', '"us"'::jsonb)
where openai_request_settings_json->'residency' = 'null'::jsonb;
