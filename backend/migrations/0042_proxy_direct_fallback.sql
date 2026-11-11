alter table runtime_settings
    add column proxy_direct_fallback_enabled boolean not null default false;
