-- 只更名，不改变 Token 的包含关系或费用语义
alter table model_requests rename column cached_tokens to cached_input_tokens;
alter table model_requests rename column cache_write_tokens to cache_write_input_tokens;
alter table model_requests rename column reasoning_tokens to reasoning_output_tokens;
alter view model_request_observations rename column cached_tokens to cached_input_tokens;
alter view model_request_observations rename column cache_write_tokens to cache_write_input_tokens;
alter view model_request_observations rename column reasoning_tokens to reasoning_output_tokens;
