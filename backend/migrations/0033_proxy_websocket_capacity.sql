alter table outbound_proxies
    add column max_websocket_connections bigint not null default 0
    check (max_websocket_connections between 0 and 4294967295);
