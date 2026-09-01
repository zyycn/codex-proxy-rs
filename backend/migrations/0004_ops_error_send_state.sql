-- 为运维错误保留 payload 发送边界与上游错误原文。
alter table model_requests
  add column raw_upstream_error text;

alter table ops_events
  add column upstream_send_state text,
  add column raw_upstream_error text,
  add constraint ops_events_upstream_send_state_ck check (
    upstream_send_state is null
    or upstream_send_state in ('not_sent', 'sent', 'ambiguous')
  );
