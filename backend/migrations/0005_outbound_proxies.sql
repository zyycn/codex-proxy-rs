create table outbound_proxies (
  id text primary key,
  name text not null,
  url text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  constraint outbound_proxies_id_ck check (
    id ~ '^pxy_[0-9a-f]{32}$'
  ),
  constraint outbound_proxies_name_ck check (
    char_length(btrim(name)) between 1 and 100
    and name = btrim(name)
    and name !~ '[[:cntrl:]]'
  ),
  constraint outbound_proxies_url_ck check (
    octet_length(url) <= 4096
    and url !~ '[[:cntrl:]]'
  ),
  constraint outbound_proxies_time_ck check (
    created_at <= updated_at
  )
);

create unique index outbound_proxies_name_uq
  on outbound_proxies (lower(name));
