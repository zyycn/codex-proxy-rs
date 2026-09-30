-- 删除 turn metadata 中 workspaces 的开关默认关闭，升级后保持既有透明转发行为。
alter table runtime_settings
    add column codex_turn_metadata_strip_workspaces boolean not null default false;
