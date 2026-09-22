-- 记录客户端请求头 x-codex-turn-state 的字节数；缺头为 NULL，与 0 字节区分。
alter table model_requests add column client_turn_state_bytes integer;
