create index if not exists idx_event_logs_created_id on event_logs(created_at desc, id desc);
create index if not exists idx_event_logs_kind_created on event_logs(kind, created_at desc);
create index if not exists idx_event_logs_request_id on event_logs(request_id);
create index if not exists idx_accounts_status on accounts(status);
create index if not exists idx_client_api_keys_prefix on client_api_keys(prefix);
create index if not exists idx_account_cookies_account on account_cookies(account_id);
