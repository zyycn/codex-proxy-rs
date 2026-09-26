-- 归属与稳定资源键属于宿主；升级沿用实例，删除实例只解除归属，不删除业务资源。
create table plugin_group_resources (
    instance_id uuid not null references plugin_instances(id) on delete cascade,
    resource_key text not null check (resource_key ~ '^[a-z0-9][a-z0-9_.-]{0,63}$'),
    group_id text not null unique references account_groups(id) on delete cascade,
    primary key (instance_id, resource_key)
);

create table plugin_key_resources (
    instance_id uuid not null references plugin_instances(id) on delete cascade,
    resource_key text not null check (resource_key ~ '^[a-z0-9][a-z0-9_.-]{0,63}$'),
    key_id text not null unique references client_api_keys(id) on delete cascade,
    primary key (instance_id, resource_key)
);
