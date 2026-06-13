alter table accounts add column quota_limit_reached integer not null default 0;
alter table accounts add column quota_cooldown_until text;
alter table accounts add column cloudflare_cooldown_until text;
