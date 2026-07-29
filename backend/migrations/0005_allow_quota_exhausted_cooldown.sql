alter table provider_accounts
  drop constraint provider_accounts_cooldown_ck,
  add constraint provider_accounts_cooldown_ck check (
    (
      availability != 'cooldown'
      or cooldown_until is not null
    )
    and (
      cooldown_until is null
      or availability in ('cooldown', 'quota_exhausted')
    )
  );
