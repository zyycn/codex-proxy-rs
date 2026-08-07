-- Initial OAuth exchange requires an id_token, but its ChatGPT user claim is optional.
alter table provider_accounts
  alter column upstream_user_id drop not null;
