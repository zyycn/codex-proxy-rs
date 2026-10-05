alter table account_groups add column fast_mode text not null default 'default'
    check (fast_mode in ('default', 'enabled', 'disabled'));

update account_groups set fast_mode = 'disabled' where disable_fast;

alter table account_groups drop column disable_fast;
