-- 保留默认调度行为；评分系数与回切偏好随运行设置原子发布。
alter table runtime_settings add column smart_scheduling_json jsonb not null default
    '{"loadWeight":1.0,"quotaWeight":0.8,"healthWeight":1.0,"latencyWeight":0.5,"resetWeight":0.0,"queueWeight":0.0,"preferHigherWeight":false}'::jsonb
    check (jsonb_typeof(smart_scheduling_json) = 'object');
