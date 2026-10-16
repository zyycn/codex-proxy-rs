-- 删除自动回切配置，保留既有智能调度系数
UPDATE runtime_settings
SET smart_scheduling_json = smart_scheduling_json - 'preferHigherWeight'
WHERE smart_scheduling_json ? 'preferHigherWeight';

ALTER TABLE runtime_settings
    ALTER COLUMN smart_scheduling_json SET DEFAULT
    '{"loadWeight":1.0,"quotaWeight":0.8,"healthWeight":1.0,"latencyWeight":0.5,"resetWeight":0.0,"queueWeight":0.0}'::jsonb;
