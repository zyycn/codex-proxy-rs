-- 隐私规则默认关闭，保留既有请求行为
alter table runtime_settings
    add column codex_privacy_policy_json jsonb not null
    default '{"enabled":false,"onError":"skip_rule","rules":[]}'::jsonb
    check (
        jsonb_typeof(codex_privacy_policy_json) = 'object'
        and jsonb_typeof(codex_privacy_policy_json->'enabled') = 'boolean'
        and codex_privacy_policy_json->>'onError' in ('skip_rule', 'reject_request')
        and jsonb_typeof(codex_privacy_policy_json->'rules') = 'array'
        and codex_privacy_policy_json ?& array['enabled', 'onError', 'rules']
    );
