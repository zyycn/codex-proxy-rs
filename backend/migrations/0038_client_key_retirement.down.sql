-- 已使用的退役标记不能丢失，避免数据库回退隐式恢复永久撤权资源
lock table client_api_keys in access exclusive mode;
do $$
begin
    if exists (select 1 from client_api_keys where retired_at is not null) then
        raise exception 'Retired client keys exist; keep a retirement-capable host and schema';
    end if;
end;
$$;
alter table client_api_keys
    drop constraint client_api_keys_retired_disabled_ck,
    drop column retired_at;
