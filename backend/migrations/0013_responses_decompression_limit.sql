-- 解压输出上限属于运行设置，升级后沿用原有 64 MiB 默认值。
alter table runtime_settings
    add column responses_max_decompressed_body_bytes bigint not null default 67108864
    check (responses_max_decompressed_body_bytes > 0);
