-- 两种调度策略分别保存窗口偏好，全零沿用最早重置优先
alter table runtime_settings
    add column quota_reset_window_weights_json jsonb not null
    default '{"fiveHour":0,"weekly":0,"monthly":0}'::jsonb;
