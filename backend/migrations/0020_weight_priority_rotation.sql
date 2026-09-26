-- 扩展可选调度策略，保留已有部署的选择与账号设置。
alter table runtime_settings drop constraint runtime_settings_rotation_ck;
alter table runtime_settings add constraint runtime_settings_rotation_ck check (
  rotation_strategy in ('smart', 'weight_priority', 'quota_reset_priority', 'round_robin', 'sticky')
);
