alter table client_api_keys
  add column starts_at timestamptz,
  add column expires_at timestamptz,
  add constraint client_api_keys_validity_order check (
    (starts_at is null and expires_at is null) or
    (starts_at is not null and expires_at is not null and starts_at < expires_at)
  );

alter table runtime_settings
  add column proxy_test_targets jsonb not null default '[]'::jsonb,
  add column proxy_test_targets_revision bigint not null default 1 check (proxy_test_targets_revision > 0);
