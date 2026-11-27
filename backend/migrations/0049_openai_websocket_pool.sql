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
