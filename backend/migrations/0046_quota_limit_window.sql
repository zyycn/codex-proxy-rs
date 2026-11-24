-- 上游明确指出耗尽窗口时，随该次拒绝保留其时长
alter table provider_accounts
    add column quota_limit_window_minutes integer
    check (quota_limit_window_minutes between 1 and 65535);
