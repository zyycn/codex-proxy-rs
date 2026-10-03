-- 每日预激活槽在执行前领取，进程重启与夏令时回拨不会重复发送上游请求。
create table account_warmup_slots (
    timezone text not null,
    local_slot timestamp without time zone not null,
    claimed_at timestamptz not null default now(),
    primary key (timezone, local_slot)
);
