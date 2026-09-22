alter table runtime_settings
  drop constraint runtime_settings_refresh_ck,
  add constraint runtime_settings_refresh_ck check (
    refresh_margin_seconds > 0
    and refresh_concurrency > 0
    and max_concurrent_per_account >= 0
    and request_interval_ms >= 0
  );
