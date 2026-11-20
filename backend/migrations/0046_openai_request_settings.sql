-- 默认启用 OAuth 同步审核专用模型，请求国家跟随位置
alter table runtime_settings
  add column openai_oauth_guardian_review_model boolean not null default true,
  add column openai_request_country_json jsonb not null default '{"mode":"follow_location"}'::jsonb,
  add constraint runtime_settings_openai_request_country_ck check (
    openai_request_country_json in ('{"mode":"default"}', '{"mode":"passthrough"}', '{"mode":"follow_location"}')
    or (
      openai_request_country_json ?& array['mode', 'country']
      and openai_request_country_json = jsonb_build_object('mode', 'custom', 'country', openai_request_country_json->>'country')
      and (openai_request_country_json->>'country') collate "C" ~ '^[A-Z]{2}$'
      and jsonb_typeof(openai_request_country_json->'country') = 'string'
    )
  );
