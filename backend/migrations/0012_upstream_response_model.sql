-- 返回模型独立于发送模型，避免展示观测改写路由与统计口径。
ALTER TABLE model_requests ADD COLUMN upstream_response_model text;

-- 历史记录只回填已经保存的明确报告，不从请求模型或映射模型推断。
UPDATE model_requests
SET upstream_response_model = btrim(provider_observation_json->>'upstreamReportedModel')
WHERE provider_kind = 'openai'
  AND jsonb_typeof(provider_observation_json->'upstreamReportedModel') = 'string'
  AND octet_length(btrim(provider_observation_json->>'upstreamReportedModel')) BETWEEN 1 AND 256
  AND provider_observation_json->>'upstreamReportedModel' !~ '[[:cntrl:]]';
