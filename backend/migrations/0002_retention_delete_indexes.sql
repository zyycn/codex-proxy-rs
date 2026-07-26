-- 保留期清理的删除谓词索引：清理任务按终态时间批量删除，
-- 无索引时每轮都会全表扫描。admin_audit_events 已有 (created_at desc, id desc)
-- 索引覆盖其删除谓词，不再重复建。
--
-- 索引在启动迁移事务内构建，期间对目标表持 SHARE 锁（挡写不挡读）。
-- 单实例部署在 serve 之前执行，无并发写；多实例滚动部署应在升级前
-- 手动 `create index concurrently` 预建同名索引，本迁移随后幂等跳过。

create index if not exists idx_model_requests_retention
    on model_requests (completed_at)
    where outcome <> 'running';

create index if not exists idx_ops_events_retention
    on ops_events (created_at)
    where model_request_id is null;
