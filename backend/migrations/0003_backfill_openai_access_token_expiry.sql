-- Direct OpenAI OAuth imports between 3.1.15 and 3.1.16 persisted JWT access
-- tokens but accidentally omitted their derived expiry. Recover only future,
-- standard JWT expiries; malformed/opaque tokens remain unchanged so this
-- one-shot repair never fabricates a credential lifetime or refresh schedule.

create function _cpr_openai_access_token_expiry(access_token text)
returns timestamptz
language plpgsql
immutable
strict
as $$
declare
  payload text;
  claims jsonb;
  epoch_seconds bigint;
begin
  if access_token !~ '^[^.]+[.][^.]+[.][^.]+$' then
    return null;
  end if;

  payload := split_part(access_token, '.', 2);
  payload := rpad(
    translate(payload, '-_', '+/'),
    ((length(payload) + 3) / 4) * 4,
    '='
  );
  claims := convert_from(decode(payload, 'base64'), 'UTF8')::jsonb;
  if jsonb_typeof(claims -> 'exp') <> 'number' then
    return null;
  end if;

  epoch_seconds := (claims ->> 'exp')::bigint;
  return to_timestamp(epoch_seconds::double precision);
exception
  when others then
    return null;
end;
$$;

with candidates as (
  select
    account.id,
    clock_timestamp() as observed_at,
    _cpr_openai_access_token_expiry(
      account.provider_credentials_json ->> 'access_token'
    ) as access_token_expires_at
  from provider_accounts as account
  where account.provider_kind = 'openai'
    and account.authentication_kind = 'oauth'
    and account.has_refresh_token
    and account.access_token_expires_at is null
    and account.next_refresh_at is null
    and account.availability not in ('expired', 'invalid', 'banned')
    and jsonb_typeof(account.provider_credentials_json -> 'access_token') = 'string'
)
update provider_accounts as account
set
  access_token_expires_at = candidates.access_token_expires_at,
  updated_at = greatest(account.updated_at, candidates.observed_at)
from candidates
where account.id = candidates.id
  and candidates.access_token_expires_at > candidates.observed_at
  and account.provider_kind = 'openai'
  and account.authentication_kind = 'oauth'
  and account.has_refresh_token
  and account.access_token_expires_at is null
  and account.next_refresh_at is null
  and account.availability not in ('expired', 'invalid', 'banned');

drop function _cpr_openai_access_token_expiry(text);
