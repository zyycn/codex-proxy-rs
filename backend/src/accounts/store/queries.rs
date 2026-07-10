//! PostgreSQL 账号仓储 SQL。

// ============================================================================
// SQL 常量
// ============================================================================

pub(super) const LIST_POOL_ACCOUNTS_SQL: &str = r"
select
  id, email, chatgpt_account_id as account_id, chatgpt_user_id as user_id,
  label, plan_type, access_token, refresh_token, access_token_expires_at,
  next_refresh_at, status, quota_limit_reached, quota_verify_required,
  quota_cooldown_until, quota_json, cloudflare_cooldown_until, added_at
from accounts
order by added_at asc, id asc";

pub(super) const GET_POOL_ACCOUNT_SQL: &str = r"
select
  id, email, chatgpt_account_id as account_id, chatgpt_user_id as user_id,
  label, plan_type, access_token, refresh_token, access_token_expires_at,
  next_refresh_at, status, quota_limit_reached, quota_verify_required,
  quota_cooldown_until, quota_json, cloudflare_cooldown_until, added_at
from accounts
where id = $1";

pub(super) const INSERT_ACCOUNT_SQL: &str = r"
insert into accounts (
  id,
  email,
  chatgpt_account_id,
  chatgpt_user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)";

pub(super) const SELECT_STORED_ACCOUNT_BY_ID_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
  status,
  added_at,
  updated_at
from accounts
where id = $1";

pub(super) const SELECT_STORED_ACCOUNT_BY_CHATGPT_IDENTITY_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
  status,
  added_at,
  updated_at
from accounts
where chatgpt_account_id = $1
  and ((chatgpt_user_id is null and $2 is null) or chatgpt_user_id = $3)
order by added_at asc
limit 1";

pub(super) const SELECT_ACCOUNT_METADATA_BY_ID_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  (refresh_token is not null and refresh_token <> '') as has_refresh_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where id = $1";

pub(super) const UPDATE_ACCOUNT_METADATA_SQL: &str = r"
update accounts
set
  email = case when $1 then $2 else email end,
  chatgpt_account_id = case when $3 then $4 else chatgpt_account_id end,
  chatgpt_user_id = case when $5 then $6 else chatgpt_user_id end,
  label = case when $7 then $8 else label end,
  plan_type = case when $9 then $10 else plan_type end,
  status = case
    when $11 then case
      when $12 = 'active' and quota_limit_reached then 'quota_exhausted'
      else $13
    end
    else status
  end,
  updated_at = $14
where id = $15";

pub(super) const LIST_STORED_ACCOUNTS_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
  status,
  added_at,
  updated_at
from accounts
where added_at < $1
  or (added_at = $2 and id < $3)
order by added_at desc, id desc
limit $4";

pub(super) const LIST_STORED_ACCOUNTS_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token,
  refresh_token,
  access_token_expires_at,
  next_refresh_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc
limit $1";

pub(super) const LIST_ACCOUNT_METADATA_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  (refresh_token is not null and refresh_token <> '') as has_refresh_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where added_at < $1
  or (added_at = $2 and id < $3)
order by added_at desc, id desc
limit $4";

pub(super) const LIST_ACCOUNT_METADATA_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  (refresh_token is not null and refresh_token <> '') as has_refresh_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc
limit $1";

pub(super) const LIST_ACCOUNT_METADATA_SELECT_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  (refresh_token is not null and refresh_token <> '') as has_refresh_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts";

pub(super) const LIST_QUOTA_SNAPSHOTS_SQL: &str = r"
select
  id,
  email,
  quota_json,
  quota_fetched_at
from accounts
where quota_json is not null
order by quota_fetched_at desc nulls last, id desc";

pub(super) const UPDATE_QUOTA_JSON_SQL: &str = r"
update accounts
set
  quota_json = $1,
  quota_fetched_at = $2,
  plan_type = coalesce($3, plan_type),
  updated_at = $4
where id = $5";

pub(super) const APPLY_QUOTA_SNAPSHOT_SQL: &str = r"
update accounts
set
  quota_json = $1,
  quota_fetched_at = $2,
  plan_type = coalesce($3, plan_type),
  status = case
    when status in ('disabled', 'banned', 'expired') then status
    when $4 then 'quota_exhausted'
    when status = 'quota_exhausted' then 'active'
    else status
  end,
  quota_limit_reached = $5,
  quota_verify_required = false,
  quota_cooldown_until = $6,
  updated_at = $7
where id = $8";

pub(super) const MARK_QUOTA_LIMITED_UNTIL_SQL: &str = r"
update accounts
set
  status = case
    when status in ('disabled', 'banned', 'expired') then status
    else 'quota_exhausted'
  end,
  quota_limit_reached = true,
  quota_verify_required = false,
  quota_cooldown_until = $1,
  updated_at = $2
where id = $3";

pub(super) const SYNC_RUNTIME_ACCOUNT_STATE_SQL: &str = r"
update accounts
set
  status = case
    when status in ('disabled', 'banned') then status
    when (
      case
        when not $1 and quota_cooldown_until is not null and quota_cooldown_until > $2 then quota_limit_reached
        else $3
      end
    ) then 'quota_exhausted'
    else $4
  end,
  quota_limit_reached = case
    when not $5 and quota_cooldown_until is not null and quota_cooldown_until > $6 then quota_limit_reached
    else $7
  end,
  quota_verify_required = case
    when $8 and quota_cooldown_until is not null and quota_cooldown_until > $9 then quota_verify_required
    else $10
  end,
  quota_cooldown_until = case
    when $11 is null and quota_cooldown_until is not null and quota_cooldown_until > $12 then quota_cooldown_until
    else $13
  end,
  cloudflare_cooldown_until = case
    when $14 is null and cloudflare_cooldown_until is not null and cloudflare_cooldown_until > $15 then cloudflare_cooldown_until
    else $16
  end,
  updated_at = $17
where id = $18";

pub(super) const SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL: &str = r"
update accounts
set
  cloudflare_cooldown_until = $1,
  updated_at = $2
where id = $3";

pub(super) const UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL: &str = r"
update accounts
set
  email = coalesce($1, email),
  chatgpt_account_id = coalesce($2, chatgpt_account_id),
  chatgpt_user_id = coalesce($3, chatgpt_user_id),
  plan_type = coalesce($4, plan_type),
  access_token = $5,
  refresh_token = $6,
  access_token_expires_at = $7,
  next_refresh_at = $8,
  status = case
    when status in ('disabled', 'banned') then status
    when quota_limit_reached then 'quota_exhausted'
    else $9
  end,
  updated_at = $10
where id = $11";

pub(super) const UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = coalesce($1, email),
  chatgpt_account_id = coalesce($2, chatgpt_account_id),
  chatgpt_user_id = coalesce($3, chatgpt_user_id),
  plan_type = coalesce($4, plan_type),
  access_token = $5,
  access_token_expires_at = $6,
  next_refresh_at = $7,
  status = case
    when status in ('disabled', 'banned') then status
    when quota_limit_reached then 'quota_exhausted'
    else $8
  end,
  updated_at = $9
where id = $10";

pub(super) const SET_NEXT_REFRESH_AT_SQL: &str = r"
update accounts
set
  next_refresh_at = $1,
  updated_at = $2
where id = $3";

pub(super) const UPDATE_IMPORTED_ACCOUNT_WITH_REFRESH_SQL: &str = r"
update accounts
set
  email = $1,
  chatgpt_account_id = $2,
  chatgpt_user_id = $3,
  label = $4,
  plan_type = $5,
  access_token = $6,
  refresh_token = $7,
  access_token_expires_at = $8,
  status = $9,
  quota_json = coalesce($10, quota_json),
  quota_fetched_at = case when $11 is null then quota_fetched_at else $12 end,
  quota_limit_reached = false,
  quota_cooldown_until = null,
  quota_verify_required = $13,
  updated_at = $14
where id = $15";

pub(super) const UPDATE_IMPORTED_ACCOUNT_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = $1,
  chatgpt_account_id = $2,
  chatgpt_user_id = $3,
  label = $4,
  plan_type = $5,
  access_token = $6,
  access_token_expires_at = $7,
  status = $8,
  quota_json = coalesce($9, quota_json),
  quota_fetched_at = case when $10 is null then quota_fetched_at else $11 end,
  quota_limit_reached = false,
  quota_cooldown_until = null,
  quota_verify_required = $12,
  updated_at = $13
where id = $14";

pub(super) const APPLY_IMPORTED_QUOTA_STATE_SQL: &str = r"
update accounts
set
  quota_json = coalesce($1, quota_json),
  quota_fetched_at = case when $2 is null then quota_fetched_at else $3 end,
  plan_type = coalesce($4, plan_type),
  quota_limit_reached = false,
  quota_cooldown_until = null,
  quota_verify_required = $5,
  updated_at = $6
where id = $7";

pub(super) const DELETE_ACCOUNT_SQL: &str = "delete from accounts where id = $1";
