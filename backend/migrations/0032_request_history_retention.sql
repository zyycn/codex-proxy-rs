-- 请求明细与统计共用 1～31 天保留期，已有超限设置收敛到 31 天
alter table runtime_settings drop constraint runtime_settings_retention_ck;
alter table runtime_settings alter column usage_retention_days set default 31;
update runtime_settings set usage_retention_days = 31 where usage_retention_days > 31;
alter table runtime_settings add constraint runtime_settings_retention_ck check (
    usage_retention_days between 1 and 31
    and ops_event_retention_days > 0
    and audit_retention_days > 0
);

-- 删除与新增、修改共用异步投影，读取已发布统计直到 Worker 原子撤销旧贡献
-- 缓存代次只在统计发布时推进，源清理不使仍然一致的统计快照失效
drop trigger request_analytics_delete_invalidation on model_requests;
drop trigger request_analytics_ops_delete_invalidation on ops_events;
drop function invalidate_deleted_request_analytics();
drop function invalidate_deleted_ops_analytics();

create function enqueue_request_analytics(request_ref text)
returns void language sql as $$
    insert into request_analytics_pending_requests(request_id)
    select request_ref where request_ref is not null
    on conflict (request_id) do update
    set change_version = nextval('request_analytics_change_version')
$$;

create or replace function observe_request_analytics_change() returns trigger language plpgsql as $$
begin
    if TG_OP = 'DELETE' then
        perform enqueue_request_analytics(OLD.id);
        return OLD;
    end if;
    perform enqueue_request_analytics(NEW.id);
    return NEW;
end
$$;

create or replace function observe_ops_analytics_change() returns trigger language plpgsql as $$
begin
    if TG_OP <> 'INSERT' then
        perform enqueue_request_analytics(OLD.model_request_id);
    end if;
    if TG_OP <> 'DELETE' then
        perform enqueue_request_analytics(NEW.model_request_id);
        return NEW;
    end if;
    return OLD;
end
$$;

-- 旧时间桶由已发布事实定位，不再维护只用于阻断读取的删除标记与索引
drop function enqueue_request_analytics(text, timestamptz, boolean);
alter table request_analytics_pending_requests
    drop column previous_bucket,
    drop column deletion_pending;
