-- 名称和模型披露保持显式授权，调度启用状态默认随额度共享
alter table client_api_keys
    add column quota_sharing_include_names boolean not null default false,
    add column quota_sharing_include_status boolean not null default true,
    add column quota_sharing_include_models boolean not null default false;
