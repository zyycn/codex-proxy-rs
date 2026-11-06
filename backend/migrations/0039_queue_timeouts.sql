-- 两层排队分别沿用实例已配置的时限，账号队满或超时默认返回错误
ALTER TABLE runtime_settings
    RENAME COLUMN concurrency_wait_timeout_seconds TO key_queue_timeout_seconds;

ALTER TABLE runtime_settings
    ADD COLUMN account_queue_timeout_seconds BIGINT NOT NULL DEFAULT 30
        CHECK (account_queue_timeout_seconds BETWEEN 1 AND 120),
    ADD COLUMN switch_account_on_queue_failure BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE runtime_settings SET account_queue_timeout_seconds = key_queue_timeout_seconds;
