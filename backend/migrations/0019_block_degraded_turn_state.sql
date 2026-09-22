-- 全局开关：上游返回恰好 312 字节 x-codex-turn-state 时不把该响应发给客户端。默认关闭。
alter table runtime_settings add column block_degraded_turn_state boolean not null default false;
