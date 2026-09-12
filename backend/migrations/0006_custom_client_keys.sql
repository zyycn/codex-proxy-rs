-- 保留旧 Key，允许迁入不同前缀、长度和标点；约束与 HTTP Bearer 可见字符一致。
alter table client_api_keys
  drop constraint client_api_keys_key_ck,
  add constraint client_api_keys_key_ck check (
    key <> '' and key collate "C" !~ '[^!-~]'
  );

-- 可见 ASCII 在 UTF-8 中编码固定，因此这里可声明 immutable。
-- 对完整值建固定大小的摘要索引，避免长 Key 超过 PostgreSQL B-tree 索引项上限。
create function client_api_key_sha256(value text) returns bytea
  language sql immutable strict parallel safe
  return sha256(convert_to(value, 'UTF8'));

create unique index client_api_keys_key_sha256_idx
  on client_api_keys (client_api_key_sha256(key));

alter table client_api_keys drop constraint client_api_keys_key_key;
