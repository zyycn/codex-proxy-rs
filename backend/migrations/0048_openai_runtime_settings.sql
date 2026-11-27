-- 按请求职责归组 OpenAI 设置，保留既有国家策略和 OAuth 审核模型开关
alter table runtime_settings
  add column openai_request_settings_json jsonb not null default '{"requestCountry":{"mode":"follow_location"},"oauthGuardianReviewModel":true,"residency":"us","accountRoutingOverride":null}'::jsonb;

update runtime_settings
set openai_request_settings_json = jsonb_build_object(
  'requestCountry', openai_request_country_json,
  'oauthGuardianReviewModel', openai_oauth_guardian_review_model,
  'residency', 'us',
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

alter table runtime_settings
  add column openai_websocket_pool_json jsonb not null default '{"maxAgeMs":3300000,"maxConnecting":8,"fastPathBudgetMs":800,"idleTimeoutMs":300000,"streamIdleTimeoutMs":300000}'::jsonb;

alter table runtime_settings
  add constraint runtime_settings_openai_websocket_pool_ck check (
    jsonb_typeof(openai_websocket_pool_json) = 'object'
    and openai_websocket_pool_json ?& array['maxAgeMs', 'maxConnecting', 'fastPathBudgetMs', 'idleTimeoutMs', 'streamIdleTimeoutMs']
    and openai_websocket_pool_json - array['maxAgeMs', 'maxConnecting', 'fastPathBudgetMs', 'idleTimeoutMs', 'streamIdleTimeoutMs'] = '{}'::jsonb
    and jsonb_typeof(openai_websocket_pool_json->'maxAgeMs') = 'number'
    and (openai_websocket_pool_json->>'maxAgeMs')::numeric between 1 and 18446744073709551615
    and (openai_websocket_pool_json->>'maxAgeMs')::numeric = trunc((openai_websocket_pool_json->>'maxAgeMs')::numeric)
    and jsonb_typeof(openai_websocket_pool_json->'maxConnecting') = 'number'
    and (openai_websocket_pool_json->>'maxConnecting')::numeric between 1 and 4294967295
    and (openai_websocket_pool_json->>'maxConnecting')::numeric = trunc((openai_websocket_pool_json->>'maxConnecting')::numeric)
    and jsonb_typeof(openai_websocket_pool_json->'fastPathBudgetMs') = 'number'
    and (openai_websocket_pool_json->>'fastPathBudgetMs')::numeric between 1 and 15000
    and (openai_websocket_pool_json->>'fastPathBudgetMs')::numeric = trunc((openai_websocket_pool_json->>'fastPathBudgetMs')::numeric)
    and jsonb_typeof(openai_websocket_pool_json->'idleTimeoutMs') = 'number'
    and (openai_websocket_pool_json->>'idleTimeoutMs')::numeric between 1 and 18446744073709551615
    and (openai_websocket_pool_json->>'idleTimeoutMs')::numeric = trunc((openai_websocket_pool_json->>'idleTimeoutMs')::numeric)
    and jsonb_typeof(openai_websocket_pool_json->'streamIdleTimeoutMs') = 'number'
    and (openai_websocket_pool_json->>'streamIdleTimeoutMs')::numeric between 0 and 18446744073709551615
    and (openai_websocket_pool_json->>'streamIdleTimeoutMs')::numeric = trunc((openai_websocket_pool_json->>'streamIdleTimeoutMs')::numeric)
  );
