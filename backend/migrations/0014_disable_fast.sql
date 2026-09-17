-- Fast 限制默认关闭；旧客户端省略更新字段时保留已有策略。
ALTER TABLE runtime_settings ADD COLUMN disable_fast boolean NOT NULL DEFAULT false;
ALTER TABLE account_groups ADD COLUMN disable_fast boolean NOT NULL DEFAULT false;
