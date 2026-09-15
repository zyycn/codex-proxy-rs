-- 排队策略随运行参数发布；默认关闭，已有实例升级后保持显式启用。
alter table runtime_settings
    add column max_waiting_per_key bigint not null default 0
        check (max_waiting_per_key between 0 and 1000),
    add column max_waiting_per_account bigint not null default 0
        check (max_waiting_per_account between 0 and 1000),
    add column concurrency_wait_timeout_seconds bigint not null default 30
        check (concurrency_wait_timeout_seconds between 1 and 120);
