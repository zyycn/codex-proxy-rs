create table outbound_proxies (
    id text primary key,
    name text not null check (char_length(name) between 1 and 100),
    proxy_url text not null,
    revision bigint not null default 1 check (revision > 0),
    last_test_at timestamptz,
    last_test_success boolean,
    last_test_latency_ms bigint,
    last_test_ip text,
    last_test_message text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

alter table provider_accounts add column outbound_proxy_id text
    references outbound_proxies(id) on delete restrict;
create index provider_accounts_outbound_proxy_id_idx on provider_accounts(outbound_proxy_id);

insert into outbound_proxies (id, name, proxy_url)
select 'proxy_' || gen_random_uuid()::text,
       'Imported proxy ' || row_number() over (order by proxy_url), proxy_url
from (select distinct outbound_proxy_url as proxy_url from provider_accounts
      where outbound_proxy_url is not null) existing;

update provider_accounts a set outbound_proxy_id = p.id
from outbound_proxies p where p.proxy_url = a.outbound_proxy_url;
