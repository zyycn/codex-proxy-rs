-- Separate credential identity facts from quota access facts. The public account
-- status remains derived at read time because Redis cooldowns and reset times are
-- time-dependent.

alter table provider_accounts
  rename column availability to credential_state;

alter table provider_accounts
  rename column availability_observed_at to credential_observed_at;

alter table provider_accounts
  drop constraint provider_accounts_availability_ck;

alter table provider_accounts
  add constraint provider_accounts_credential_state_ck check (
    credential_state in ('unknown', 'ready', 'expired', 'banned', 'invalid')
  ) not valid;

alter table provider_accounts
  add column quota_access_state text not null default 'unknown',
  add column quota_evidence text,
  add column quota_access_observed_at timestamptz,
  add column quota_reset_at timestamptz,
  add column last_error_reason text;

alter table provider_accounts
  drop constraint provider_accounts_quota_observation_ck;

-- Old quota_exhausted credential states are quota facts. Re-derive OpenAI quota
-- access from the raw authoritative response and never trust the old percentage-
-- derived quota_limit_reached boolean.
update provider_accounts
set
  quota_access_state = case
    when provider_kind = 'openai'
      and provider_quota_json #>> '{rate_limit,allowed}' = 'true'
      then 'allowed'
    when provider_kind = 'openai'
      and (
        provider_quota_json #>> '{rate_limit,allowed}' = 'false'
        or (
          coalesce(provider_quota_json #>> '{rate_limit,allowed}', '') <> 'true'
          and (
            provider_quota_json #>> '{rate_limit,limit_reached}' = 'true'
            or provider_quota_json #>> '{rate_limit,primary_window,limit_reached}' = 'true'
          )
        )
      )
      then 'exhausted'
    when provider_kind = 'xai' and credential_state = 'quota_exhausted'
      then 'exhausted'
    when provider_quota_json is not null then 'unknown'
    else 'unknown'
  end,
  quota_evidence = case
    when provider_kind = 'openai'
      and provider_quota_json #>> '{rate_limit,allowed}' = 'false'
      then 'provider_denied'
    when provider_kind = 'openai'
      and coalesce(provider_quota_json #>> '{rate_limit,allowed}', '') <> 'true'
      and (
        provider_quota_json #>> '{rate_limit,limit_reached}' = 'true'
        or provider_quota_json #>> '{rate_limit,primary_window,limit_reached}' = 'true'
      )
      then 'account_limit_reached'
    when provider_kind = 'xai' and credential_state = 'quota_exhausted'
      then 'usage_limit_reached'
    else null
  end,
  quota_access_observed_at = case
    when provider_kind = 'openai' and provider_quota_json is not null
      then quota_observed_at
    when provider_kind = 'xai' and credential_state = 'quota_exhausted'
      then credential_observed_at
    else null
  end,
  quota_reset_at = case
    when provider_kind = 'openai'
      and jsonb_typeof(provider_quota_json #> '{rate_limit,primary_window,reset_at}') = 'number'
      then to_timestamp((provider_quota_json #>> '{rate_limit,primary_window,reset_at}')::double precision)
    else null
  end,
  credential_state = case
    when credential_state = 'quota_exhausted' and upstream_user_id is not null then 'ready'
    when credential_state = 'quota_exhausted' then 'unknown'
    else credential_state
  end,
  last_error_reason = case credential_state
    when 'unknown' then 'account_unverified'
    when 'expired' then 'credential_expired'
    when 'invalid' then 'credential_invalid'
    when 'banned' then 'account_banned'
    else null
  end,
  last_error_message = case
    when credential_state in ('unknown', 'expired', 'invalid', 'banned') then last_error_message
    else null
  end;

alter table provider_accounts
  validate constraint provider_accounts_credential_state_ck;

alter table provider_accounts
  add constraint provider_accounts_quota_access_state_ck check (
    quota_access_state in ('unknown', 'allowed', 'exhausted')
  ),
  add constraint provider_accounts_quota_evidence_ck check (
    quota_evidence is null
    or quota_evidence in (
      'provider_denied',
      'account_limit_reached',
      'usage_limit_reached',
      'payment_required'
    )
  ),
  add constraint provider_accounts_quota_fact_ck check (
    (quota_access_state = 'unknown' and quota_evidence is null and quota_reset_at is null)
    or (
      quota_access_state = 'allowed'
      and quota_evidence is null
      and quota_access_observed_at is not null
      and quota_reset_at is null
    )
    or (
      quota_access_state = 'exhausted'
      and quota_evidence is not null
      and quota_access_observed_at is not null
    )
  ),
  add constraint provider_accounts_quota_observation_ck check (
    (provider_quota_json is null) = (quota_observed_at is null)
  ),
  add constraint provider_accounts_error_reason_ck check (
    last_error_reason is null
    or last_error_reason in (
      'account_unverified',
      'access_token_expired',
      'credential_expired',
      'credential_invalid',
      'account_banned'
    )
  );

drop index provider_accounts_availability_idx;
create index provider_accounts_credential_state_idx
  on provider_accounts (credential_state, id);
create index provider_accounts_quota_access_idx
  on provider_accounts (quota_access_state, quota_reset_at, id);

alter table provider_accounts
  drop column quota_limit_reached;

alter table provider_accounts
  rename constraint provider_accounts_time_ck to provider_accounts_time_ck_old;

alter table provider_accounts
  drop constraint provider_accounts_time_ck_old;

alter table provider_accounts
  add constraint provider_accounts_time_ck check (
    created_at <= updated_at
    and credential_observed_at <= updated_at
    and (quota_access_observed_at is null or quota_access_observed_at <= updated_at)
    and (quota_observed_at is null or quota_observed_at <= updated_at)
  );
