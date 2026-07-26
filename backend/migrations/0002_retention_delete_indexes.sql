-- 保留期清理的删除谓词索引：清理任务按终态时间批量删除，
-- 无索引时每轮都会全表扫描三张审计/用量表。

create index if not exists idx_model_requests_retention
    on model_requests (completed_at)
    where outcome <> 'running';

create index if not exists idx_ops_events_retention
    on ops_events (created_at)
    where model_request_id is null;

create index if not exists idx_admin_audit_events_retention
    on admin_audit_events (created_at);
