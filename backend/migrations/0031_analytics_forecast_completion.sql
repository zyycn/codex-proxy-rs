-- 完成时间边界只回读五分钟内的窄事实，Provider 文档继续使用源表采样索引
create index request_analytics_facts_account_completion on request_analytics_facts
(provider_account_ref,completed_at,started_at)
where completed_at is not null and outcome <> 'running';

-- 新完成时间维度与既有统计共用可恢复构建，旧事实无需再次投影
insert into request_analytics_pending_buckets(bucket_start, oldest_pending_at)
select distinct date_bin(interval '15 minutes', started_at, timestamptz '2000-01-01 UTC'), now()
from request_analytics_facts
on conflict (bucket_start) do update
set change_version = nextval('request_analytics_change_version');
