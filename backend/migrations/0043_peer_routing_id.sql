-- 同一部署的实例共用转发标记，启动时读取，不随请求访问数据库
alter table runtime_settings add column peer_routing_id uuid not null default gen_random_uuid();
