create table ops_error_logs (
  id text primary key,
  request_id text,
  kind text not null,
  account_id text,
  route text,
  model text,
  status_code integer check (status_code is null or (status_code >= 100 and status_code <= 599)),
  client_status_code integer check (client_status_code is null or (client_status_code >= 100 and client_status_code <= 599)),
  upstream_status_code integer check (upstream_status_code is null or (upstream_status_code >= 100 and upstream_status_code <= 599)),
  transport text,
  attempt_index integer check (attempt_index is null or attempt_index >= 0),
  failure_class text,
  response_id text,
  upstream_request_id text,
  latency_ms integer check (latency_ms is null or latency_ms >= 0),
  message text not null,
  metadata_json text not null,
  created_at text not null
);

create index idx_ops_error_logs_created_id on ops_error_logs(created_at desc, id desc);
create index idx_ops_error_logs_request_id on ops_error_logs(request_id) where request_id is not null;
create index idx_ops_error_logs_account on ops_error_logs(account_id, created_at desc) where account_id is not null;
create index idx_ops_error_logs_route_created on ops_error_logs(route, created_at desc) where route is not null;
create index idx_ops_error_logs_model_created on ops_error_logs(model, created_at desc) where model is not null;
create index idx_ops_error_logs_status_created on ops_error_logs(status_code, created_at desc) where status_code is not null;
create index idx_ops_error_logs_transport_created on ops_error_logs(transport, created_at desc) where transport is not null;
create index idx_ops_error_logs_failure_class on ops_error_logs(failure_class, created_at desc) where failure_class is not null;
create index idx_ops_error_logs_response_id on ops_error_logs(response_id) where response_id is not null;
create index idx_ops_error_logs_upstream_request_id on ops_error_logs(upstream_request_id) where upstream_request_id is not null;
