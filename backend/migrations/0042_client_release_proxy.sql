alter table runtime_settings
    add column client_release_proxy_id text references outbound_proxies(id) on delete restrict;
