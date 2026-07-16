alter table usage_records
  add column cache_write_tokens bigint
  check (cache_write_tokens is null or cache_write_tokens >= 0);

alter table request_time_buckets
  add column cache_write_tokens bigint not null default 0
  check (cache_write_tokens >= 0);
