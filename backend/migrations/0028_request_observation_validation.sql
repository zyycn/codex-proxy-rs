-- 观测 JSON 只在插入或内容变化时校验，外键置空不重复遍历历史快照
create function check_model_request_observation() returns trigger
language plpgsql as $$
begin
  if model_request_observation_valid(new.request_observation_json) is not true then
    raise exception using
      errcode = '23514',
      message = 'request observation violates shape constraint',
      constraint = 'model_requests_observation_shape_ck',
      table = tg_table_name,
      schema = tg_table_schema;
  end if;
  return null;
end;
$$;

-- AFTER 检查最终写入值，也覆盖其他 BEFORE 触发器造成的 JSON 变化
create trigger model_requests_observation_insert
  after insert on model_requests
  for each row execute function check_model_request_observation();
create trigger model_requests_observation_update
  after update on model_requests
  for each row
  when (old.request_observation_json is distinct from new.request_observation_json)
  execute function check_model_request_observation();

alter table model_requests drop constraint model_requests_observation_shape_ck;
