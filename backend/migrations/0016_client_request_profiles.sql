-- Provider 默认选择仅在首次初始化时写入；后续以管理端保存的配置为准。
alter table runtime_settings
    add column provider_request_profiles_json jsonb not null default '{}'::jsonb,
    add constraint runtime_settings_request_profiles_object
        check (jsonb_typeof(provider_request_profiles_json) = 'object');

-- 空对象表示各 Provider 均跟随通用设置，只保存显式独立覆盖。
alter table client_api_keys
    add column provider_request_profiles_json jsonb not null default '{}'::jsonb,
    add constraint client_api_keys_request_profiles_object
        check (jsonb_typeof(provider_request_profiles_json) = 'object');
