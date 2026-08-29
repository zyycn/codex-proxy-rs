-- 请求热路径的准入判定、账号选择等待与账号池容量观测。
alter table model_requests
  add column admission_decision_ms bigint,
  add column account_selection_wait_ms bigint,
  add column capacity_used_slots bigint,
  add column capacity_total_slots bigint;

alter table model_requests
  add constraint model_requests_runtime_pressure_ck check (
    (admission_decision_ms is null or admission_decision_ms >= 0)
    and (account_selection_wait_ms is null or account_selection_wait_ms >= 0)
    and (
      (capacity_used_slots is null and capacity_total_slots is null)
      or (
        capacity_used_slots >= 0
        and capacity_total_slots > 0
        and capacity_used_slots <= capacity_total_slots
      )
    )
  );
