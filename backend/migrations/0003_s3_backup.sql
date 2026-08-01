-- S3 兼容对象存储（含 Cloudflare R2）与 PostgreSQL 备份。
-- 按 docs/s3-backup-design-audit.md 最终状态实现。
--
-- backup_settings 是单例配置行（id = 1）；backup_records 记录每次备份任务。
-- 当前部署边界是单副本：任务执行由单个可取消 Daemon 承担，记录表不需要
-- fencing token、heartbeat 或墓碑状态。skipped 只是日志/指标，删除成功后记录
-- 硬删除，操作历史进入 admin_audit_events。
-- 手动过期 TTL：每份备份独立的 expires_at，创建时确定，到期自动清理
-- （审计 A-06 曾移除该字段，按参考实现 sub2api 的语义加回）。

create table backup_settings (
  id bigint primary key,
  storage_revision bigint not null default 1,
  endpoint text,
  region text,
  bucket text,
  access_key_id text,
  secret_access_key text,
  prefix text,
  force_path_style boolean not null default false,
  schedule_enabled boolean not null default false,
  cron_expression text,
  schedule_timezone text default 'Asia/Shanghai',
  retention_days bigint not null default 0,
  retention_count bigint not null default 0,
  next_run_at timestamptz,
  last_verified_at timestamptz,
  updated_at timestamptz not null,
  constraint backup_settings_singleton_ck check (id = 1),
  constraint backup_settings_revision_ck check (storage_revision > 0),
  constraint backup_settings_endpoint_ck check (
    endpoint is null
    or (
      endpoint ~ '^https?://'
      and endpoint !~ '[[:space:][:cntrl:]]'
      and endpoint !~ '://[^/]*@'
    )
  ),
  constraint backup_settings_lengths_ck check (
    (bucket is null or (octet_length(bucket) between 1 and 255 and bucket !~ '[[:cntrl:]]'))
    and (region is null or (octet_length(region) between 1 and 64 and region !~ '[[:cntrl:]]'))
    and (prefix is null or (octet_length(prefix) <= 1024 and prefix !~ '[[:cntrl:]]'))
    and (
      access_key_id is null
      or (octet_length(access_key_id) between 1 and 255 and access_key_id !~ '[[:cntrl:]]')
    )
    and (
      secret_access_key is null
      or (
        octet_length(secret_access_key) between 1 and 512
        and secret_access_key !~ '[[:cntrl:]]'
      )
    )
  ),
  constraint backup_settings_schedule_ck check (
    (cron_expression is null or cron_expression ~ '^[^[:space:]]+([[:space:]]+[^[:space:]]+){4}$')
    and (
      schedule_timezone is null
      or (octet_length(schedule_timezone) between 1 and 64 and schedule_timezone !~ '[[:cntrl:]]')
    )
    and retention_days >= 0
    and retention_count >= 0
    and (
      not schedule_enabled
      or (
        endpoint is not null
        and bucket is not null
        and region is not null
        and access_key_id is not null
        and secret_access_key is not null
        and prefix is not null
        and cron_expression is not null
        and schedule_timezone is not null
        and last_verified_at is not null
      )
    )
  ),
  -- 计划启停与游标一致性：启用必有 next_run_at，关闭必清空。
  constraint backup_settings_cursor_ck check (
    (not schedule_enabled and next_run_at is null)
    or (schedule_enabled and next_run_at is not null)
  )
);

insert into backup_settings (
  id,
  storage_revision,
  schedule_enabled,
  retention_days,
  retention_count,
  force_path_style,
  schedule_timezone,
  updated_at
)
values (1, 1, false, 0, 0, false, 'Asia/Shanghai', now());

create table backup_records (
  id text primary key,
  trigger_kind text not null,
  status text not null,
  scheduled_at timestamptz,
  object_key text not null,
  size_bytes bigint,
  sha256 text,
  expires_at timestamptz,
  attempt_count integer not null default 0,
  error_code text,
  error_message text,
  started_at timestamptz,
  completed_at timestamptz,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  constraint backup_records_id_ck check (id ~ '^backup_[0-9a-f]{32}$'),
  constraint backup_records_trigger_kind_ck check (
    trigger_kind in ('manual', 'scheduled')
    and (
      (trigger_kind = 'manual' and scheduled_at is null)
      or (trigger_kind = 'scheduled' and scheduled_at is not null)
    )
  ),
  constraint backup_records_status_ck check (
    status in ('queued', 'dumping', 'uploading', 'completed', 'failed', 'deleting')
  ),
  constraint backup_records_attempt_ck check (attempt_count >= 0),
  constraint backup_records_lifecycle_ck check (
    created_at <= updated_at
    and (started_at is null or started_at >= created_at)
    and (completed_at is null or completed_at >= created_at)
    and (
      (
        status = 'queued'
        and started_at is null
        and completed_at is null
      )
      or (
        status in ('dumping', 'uploading')
        and started_at is not null
        and completed_at is null
      )
      or (
        status in ('completed', 'failed', 'deleting')
        and started_at is not null
        and completed_at is not null
      )
    )
  ),
  constraint backup_records_artifact_ck check (
    (
      status not in ('uploading', 'completed')
      or (size_bytes is not null and sha256 is not null)
    )
    and (size_bytes is null or size_bytes >= 0)
    and (sha256 is null or sha256 ~ '^[0-9a-f]{64}$')
  ),
  constraint backup_records_error_ck check (
    (error_code is null) = (error_message is null)
    and (status <> 'failed' or (error_code is not null and error_message is not null))
    and (
      status not in ('queued', 'dumping', 'uploading', 'completed')
      or error_code is null
    )
  ),
  -- expires_at 为空，或不早于创建时间（创建时 now + 天数 恒晚于 created_at）。
  constraint backup_records_expiry_ck check (
    expires_at is null or expires_at >= created_at
  )
);

create index backup_records_created_idx
  on backup_records (created_at desc, id desc);
create index backup_records_status_idx
  on backup_records (status, created_at, id);
create unique index backup_records_scheduled_uq
  on backup_records (scheduled_at)
  where trigger_kind = 'scheduled' and scheduled_at is not null;
create unique index backup_records_active_uq
  on backup_records ((1))
  where status in ('queued', 'dumping', 'uploading');
create unique index backup_records_object_key_uq
  on backup_records (object_key)
  where object_key is not null;
-- 同时服务 retentionDays 与 retentionCount。
create index backup_records_scheduled_completed_idx
  on backup_records (completed_at desc, id desc)
  where trigger_kind = 'scheduled' and status = 'completed';
-- 可清理记录（expires_at 已到期且非活跃）的扫描索引。
create index backup_records_retention_idx
  on backup_records (expires_at)
  where expires_at is not null and status in ('completed', 'failed');
