create table runtime_settings_new (
  id integer primary key check (id = 1),
  model_aliases_json text not null default '{}',
  refresh_margin_seconds integer not null check (refresh_margin_seconds > 0),
  refresh_concurrency integer not null check (refresh_concurrency > 0),
  max_concurrent_per_account integer not null check (max_concurrent_per_account > 0),
  request_interval_ms integer not null check (request_interval_ms >= 0),
  rotation_strategy text not null check (rotation_strategy in ('smart', 'quota_reset_priority', 'round_robin', 'sticky')),
  admin_api_key text,
  updated_at text not null
);

insert into runtime_settings_new (
  id,
  model_aliases_json,
  refresh_margin_seconds,
  refresh_concurrency,
  max_concurrent_per_account,
  request_interval_ms,
  rotation_strategy,
  admin_api_key,
  updated_at
)
select
  id,
  model_aliases_json,
  refresh_margin_seconds,
  refresh_concurrency,
  max_concurrent_per_account,
  request_interval_ms,
  case rotation_strategy
    when 'least_used' then 'quota_reset_priority'
    else rotation_strategy
  end,
  admin_api_key,
  updated_at
from runtime_settings;

drop table runtime_settings;
alter table runtime_settings_new rename to runtime_settings;
