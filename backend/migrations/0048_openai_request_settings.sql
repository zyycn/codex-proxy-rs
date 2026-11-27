-- 按请求职责归组 OpenAI 设置，保留既有国家策略和 OAuth 审核模型开关
alter table runtime_settings
  add column openai_request_settings_json jsonb not null default '{"requestCountry":{"mode":"follow_location"},"oauthGuardianReviewModel":true,"residency":null,"accountRoutingOverride":null}'::jsonb;

update runtime_settings
set openai_request_settings_json = jsonb_build_object(
  'requestCountry', openai_request_country_json,
  'oauthGuardianReviewModel', openai_oauth_guardian_review_model,
  'residency', null,
  'accountRoutingOverride', null
);

alter table runtime_settings
  drop column openai_request_country_json,
  drop column openai_oauth_guardian_review_model,
  add constraint runtime_settings_openai_request_settings_ck check (
    jsonb_typeof(openai_request_settings_json) = 'object'
    and openai_request_settings_json ?& array['requestCountry', 'oauthGuardianReviewModel', 'residency', 'accountRoutingOverride']
    and openai_request_settings_json - 'requestCountry' - 'oauthGuardianReviewModel' - 'residency' - 'accountRoutingOverride' = '{}'::jsonb
    and jsonb_typeof(openai_request_settings_json->'oauthGuardianReviewModel') = 'boolean'
    and jsonb_typeof(openai_request_settings_json->'residency') in ('null', 'string')
    and jsonb_typeof(openai_request_settings_json->'accountRoutingOverride') in ('null', 'string')
    and (
      openai_request_settings_json->'requestCountry' in ('{"mode":"default"}', '{"mode":"passthrough"}', '{"mode":"follow_location"}')
      or (
        (openai_request_settings_json->'requestCountry') ?& array['mode', 'country']
        and openai_request_settings_json->'requestCountry' = jsonb_build_object('mode', 'custom', 'country', openai_request_settings_json->'requestCountry'->>'country')
        and (openai_request_settings_json->'requestCountry'->>'country') collate "C" ~ '^[A-Z]{2}$'
        and jsonb_typeof(openai_request_settings_json->'requestCountry'->'country') = 'string'
      )
    )
  );
