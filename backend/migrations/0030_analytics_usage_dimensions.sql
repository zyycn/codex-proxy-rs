-- 完整交付用量复用现有统计状态，最近使用时间保持可合并的独立极值
alter table request_analytics_15m add column last_used_at timestamptz;
alter table request_analytics_hourly add column last_used_at timestamptz;

-- 新指标只重建时间桶，不清空窄事实，也不重置已经完成的历史投影游标
insert into request_analytics_pending_buckets(bucket_start, oldest_pending_at)
select distinct date_bin(interval '15 minutes', started_at, timestamptz '2000-01-01 UTC'), now()
from request_analytics_facts
on conflict (bucket_start) do update
set change_version = nextval('request_analytics_change_version');

-- 撤销最近一次模型贡献后按真实源定位极值，避免扫描整个账号历史
create index model_requests_delivered_account_model on model_requests
(provider_account_ref,(coalesce(upstream_model_id,requested_model_id,'')),started_at desc,id desc)
where outcome='succeeded' and downstream_committed_at is not null
    and (provider_kind is distinct from 'openai' or request_kind is distinct from 'prewarm')
    and ((client_transport='websocket' and client_status_code is null) or client_status_code between 200 and 399)
    and (requested_model_id is not null or upstream_model_id is not null or image_generation_requested
         or input_tokens is not null or output_tokens is not null or cached_input_tokens is not null
         or cache_write_input_tokens is not null or reasoning_output_tokens is not null
         or image_input_tokens is not null or image_output_tokens is not null
         or total_tokens is not null or cost_amount is not null);

-- 预测只定位各采样桶最后一份 Provider 文档，宽 JSON 不进入索引
create index model_requests_forecast_documents on model_requests
(provider_account_ref,completed_at desc,id desc) include(started_at)
where provider_observation_json is not null and outcome <> 'running';

-- 增量构建按字节序推进游标，索引排序必须与游标合同一致
create index request_analytics_facts_build_cursor on request_analytics_facts
(started_at,id collate "C");
create index request_analytics_15m_build_cursor on request_analytics_15m
(bucket_start,collection collate "C",dimensions collate "C",outcome collate "C");
