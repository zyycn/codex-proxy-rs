alter table provider_accounts
  add column concurrency_limit bigint,
  add column weight smallint not null default 1,
  add constraint provider_accounts_concurrency_limit_ck
    check (concurrency_limit is null or concurrency_limit between 1 and 4294967295),
  add constraint provider_accounts_weight_ck
    check (weight between 1 and 100);
