alter table runtime_settings
    add column proxy_test_targets jsonb not null default '[]'::jsonb,
    add column proxy_test_targets_revision bigint not null default 1 check (proxy_test_targets_revision > 0);
