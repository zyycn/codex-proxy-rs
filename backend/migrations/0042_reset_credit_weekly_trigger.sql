alter table runtime_settings
    add column reset_credit_on_low_weekly_quota boolean not null default false;
