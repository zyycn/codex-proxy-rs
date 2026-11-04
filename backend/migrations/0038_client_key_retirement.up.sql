-- 退役保留费用及插件幂等关联，旧版本启用操作也不能恢复退役凭据
alter table client_api_keys
    add column retired_at timestamptz,
    add constraint client_api_keys_retired_disabled_ck check (retired_at is null or not enabled);
