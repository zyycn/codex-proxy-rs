-- OAuth 刷新提前量收窄到官方 codex 客户端基线（exp 前 5 分钟，manager.rs
-- CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES=5）；仍为旧默认 3600 的已部署
-- 实例一并迁移，管理员自定义值保持不变。
alter table runtime_settings
  alter column refresh_margin_seconds set default 300;

update runtime_settings
  set refresh_margin_seconds = 300
  where id = 1
    and refresh_margin_seconds = 3600;
