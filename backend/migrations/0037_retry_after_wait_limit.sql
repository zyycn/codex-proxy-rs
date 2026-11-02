-- 网关接受的单次上游重试等待上限，零表示不接受正数等待
ALTER TABLE runtime_settings
    ADD COLUMN max_retry_after_seconds BIGINT NOT NULL DEFAULT 240
    CHECK (max_retry_after_seconds BETWEEN 0 AND 4294967295);
