-- Account groups are provider-neutral account sets. Existing provider-scoped
-- keys are placed in ordinary migration groups before their provider column is
-- removed, preserving their pre-upgrade account scope.

create table account_groups (
  id text primary key,
  name text not null,
  description text,
  enabled boolean not null default true,
  created_at timestamptz not null,
  updated_at timestamptz not null,
  constraint account_groups_id_ck check (
    id ~ '^grp_[0-9a-f]{32}$'
  ),
  constraint account_groups_name_ck check (
    char_length(btrim(name)) between 1 and 100
    and name = btrim(name)
    and name !~ '[[:cntrl:]]'
  ),
  constraint account_groups_description_ck check (
    description is null or (
      octet_length(description) <= 4096
      and description !~ '[[:cntrl:]]'
    )
  ),
  constraint account_groups_time_ck check (
    created_at <= updated_at
  )
);

create unique index account_groups_name_uq
  on account_groups (lower(name));

create index account_groups_list_idx
  on account_groups (enabled, created_at desc, id desc);

create table account_group_accounts (
  account_group_id text not null,
  provider_account_id text not null,
  created_at timestamptz not null,
  primary key (account_group_id, provider_account_id),
  constraint account_group_accounts_group_fk foreign key (account_group_id)
    references account_groups (id)
    on update restrict
    on delete cascade,
  constraint account_group_accounts_account_fk foreign key (provider_account_id)
    references provider_accounts (id)
    on update restrict
    on delete cascade
);

create index account_group_accounts_account_idx
  on account_group_accounts (provider_account_id, account_group_id);

create table client_api_key_groups (
  client_api_key_id text not null,
  account_group_id text not null,
  created_at timestamptz not null,
  primary key (client_api_key_id, account_group_id),
  constraint client_api_key_groups_key_fk foreign key (client_api_key_id)
    references client_api_keys (id)
    on update restrict
    on delete cascade,
  constraint client_api_key_groups_group_fk foreign key (account_group_id)
    references account_groups (id)
    on update restrict
    on delete restrict
);

create index client_api_key_groups_group_idx
  on client_api_key_groups (account_group_id, client_api_key_id);

with provider_kinds as (
  select provider_kind from provider_accounts
  union
  select provider_kind from client_api_keys
)
insert into account_groups (id, name, description, enabled, created_at, updated_at)
select
  'grp_' || md5('codex-proxy-rs:migration-group:' || provider_kind),
  '迁移池-' || provider_kind,
  '升级时按原 Provider 权限自动生成',
  true,
  now(),
  now()
from provider_kinds;

insert into account_group_accounts (account_group_id, provider_account_id, created_at)
select
  'grp_' || md5('codex-proxy-rs:migration-group:' || provider_kind),
  id,
  clock_timestamp()
from provider_accounts;

insert into client_api_key_groups (client_api_key_id, account_group_id, created_at)
select
  id,
  'grp_' || md5('codex-proxy-rs:migration-group:' || provider_kind),
  clock_timestamp()
from client_api_keys;

alter table model_requests
  add column routing_scope text not null default 'legacy_provider',
  add column routing_group_refs text[] not null default '{}',
  add column routing_group_names_snapshot jsonb not null default '[]'::jsonb,
  add constraint model_requests_routing_scope_ck check (
    routing_scope in ('legacy_provider', 'all', 'groups')
  ),
  add constraint model_requests_routing_group_names_ck check (
    jsonb_typeof(routing_group_names_snapshot) = 'array'
    and (
      (
        routing_scope in ('legacy_provider', 'all')
        and cardinality(routing_group_refs) = 0
        and jsonb_array_length(routing_group_names_snapshot) = 0
      )
      or (
        routing_scope = 'groups'
        and cardinality(routing_group_refs) > 0
        and array_position(routing_group_refs, null) is null
        and jsonb_array_length(routing_group_names_snapshot)
          = cardinality(routing_group_refs)
      )
    )
  );

create index model_requests_routing_group_refs_idx
  on model_requests using gin (routing_group_refs);

alter table model_requests
  alter column routing_scope drop default;

alter table client_api_keys
  drop constraint client_api_keys_provider_kind_ck,
  drop column provider_kind;
