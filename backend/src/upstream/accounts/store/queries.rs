//! SQLite 账号仓储 SQL。

// ============================================================================
// SQL 常量
// ============================================================================

pub(super) const LIST_POOL_ACCOUNTS_SQL: &str = r"
select
  a.id,
  a.email,
  a.chatgpt_account_id as account_id,
  a.chatgpt_user_id as user_id,
  a.label,
  a.plan_type,
  a.access_token,
  a.refresh_token,
  a.access_token_expires_at,
  a.next_refresh_at,
  a.status,
  a.quota_limit_reached,
  a.quota_verify_required,
  a.quota_cooldown_until,
  a.quota_json,
  a.cloudflare_cooldown_until,
  a.added_at,
  coalesce(au.request_count, 0) as usage_request_count,
  coalesce(au.empty_response_count, 0) as usage_empty_response_count,
  coalesce(au.image_input_tokens, 0) as usage_image_input_tokens,
  coalesce(au.image_output_tokens, 0) as usage_image_output_tokens,
  coalesce(au.image_request_count, 0) as usage_image_request_count,
  coalesce(au.image_request_failed_count, 0) as usage_image_request_failed_count,
  coalesce(au.window_request_count, 0) as usage_window_request_count,
  coalesce(au.window_input_tokens, 0) as usage_window_input_tokens,
  coalesce(au.window_output_tokens, 0) as usage_window_output_tokens,
  coalesce(au.window_cached_tokens, 0) as usage_window_cached_tokens,
  coalesce(au.window_image_input_tokens, 0) as usage_window_image_input_tokens,
  coalesce(au.window_image_output_tokens, 0) as usage_window_image_output_tokens,
  coalesce(au.window_image_request_count, 0) as usage_window_image_request_count,
  coalesce(au.window_image_request_failed_count, 0) as usage_window_image_request_failed_count,
  au.window_started_at as usage_window_started_at,
  au.window_reset_at as usage_window_reset_at,
  au.limit_window_seconds as usage_limit_window_seconds,
  au.last_used_at as usage_last_used_at
from accounts a
left join account_usage au on au.account_id = a.id
order by a.rowid asc";

pub(super) const GET_POOL_ACCOUNT_SQL: &str = r"
select
  a.id,
  a.email,
  a.chatgpt_account_id as account_id,
  a.chatgpt_user_id as user_id,
  a.label,
  a.plan_type,
  a.access_token,
  a.refresh_token,
  a.access_token_expires_at,
  a.next_refresh_at,
  a.status,
  a.quota_limit_reached,
  a.quota_verify_required,
  a.quota_cooldown_until,
  a.quota_json,
  a.cloudflare_cooldown_until,
  a.added_at,
  coalesce(au.request_count, 0) as usage_request_count,
  coalesce(au.empty_response_count, 0) as usage_empty_response_count,
  coalesce(au.image_input_tokens, 0) as usage_image_input_tokens,
  coalesce(au.image_output_tokens, 0) as usage_image_output_tokens,
  coalesce(au.image_request_count, 0) as usage_image_request_count,
  coalesce(au.image_request_failed_count, 0) as usage_image_request_failed_count,
  coalesce(au.window_request_count, 0) as usage_window_request_count,
  coalesce(au.window_input_tokens, 0) as usage_window_input_tokens,
  coalesce(au.window_output_tokens, 0) as usage_window_output_tokens,
  coalesce(au.window_cached_tokens, 0) as usage_window_cached_tokens,
  coalesce(au.window_image_input_tokens, 0) as usage_window_image_input_tokens,
  coalesce(au.window_image_output_tokens, 0) as usage_window_image_output_tokens,
  coalesce(au.window_image_request_count, 0) as usage_window_image_request_count,
  coalesce(au.window_image_request_failed_count, 0) as usage_window_image_request_failed_count,
  au.window_started_at as usage_window_started_at,
  au.window_reset_at as usage_window_reset_at,
  au.limit_window_seconds as usage_limit_window_seconds,
  au.last_used_at as usage_last_used_at
from accounts a
left join account_usage au on au.account_id = a.id
where a.id = ?";

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
) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

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
where id = ?";

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
where chatgpt_account_id = ?
  and ((chatgpt_user_id is null and ? is null) or chatgpt_user_id = ?)
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
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where id = ?";

pub(super) const UPDATE_ACCOUNT_METADATA_SQL: &str = r"
update accounts
set
  email = case when ? then ? else email end,
  chatgpt_account_id = case when ? then ? else chatgpt_account_id end,
  chatgpt_user_id = case when ? then ? else chatgpt_user_id end,
  label = case when ? then ? else label end,
  plan_type = case when ? then ? else plan_type end,
  status = case when ? then ? else status end,
  updated_at = ?
where id = ?";

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
where added_at < ?
  or (added_at = ? and id < ?)
order by added_at desc, id desc
limit ?";

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
limit ?";

pub(super) const LIST_ACCOUNT_METADATA_AFTER_CURSOR_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
where added_at < ?
  or (added_at = ? and id < ?)
order by added_at desc, id desc
limit ?";

pub(super) const LIST_ACCOUNT_METADATA_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts
order by added_at desc, id desc
limit ?";

pub(super) const LIST_ACCOUNT_METADATA_SELECT_SQL: &str = r"
select
  id,
  email,
  chatgpt_account_id as account_id,
  chatgpt_user_id as user_id,
  label,
  plan_type,
  access_token_expires_at,
  status,
  added_at,
  updated_at
from accounts";

pub(super) const RECORD_USAGE_SQL: &str = r"
insert into account_usage (
  account_id,
  request_count,
  empty_response_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  reasoning_tokens,
  total_tokens,
  image_input_tokens,
  image_output_tokens,
  image_request_count,
  image_request_failed_count,
  window_request_count,
  window_input_tokens,
  window_output_tokens,
  window_cached_tokens,
  window_image_input_tokens,
  window_image_output_tokens,
  window_image_request_count,
  window_image_request_failed_count,
  window_started_at,
  last_used_at
) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(account_id) do update set
  request_count = request_count + excluded.request_count,
  empty_response_count = empty_response_count + excluded.empty_response_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
  reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
  total_tokens = total_tokens + excluded.total_tokens,
  image_input_tokens = image_input_tokens + excluded.image_input_tokens,
  image_output_tokens = image_output_tokens + excluded.image_output_tokens,
  image_request_count = image_request_count + excluded.image_request_count,
  image_request_failed_count = image_request_failed_count + excluded.image_request_failed_count,
  window_request_count = window_request_count + excluded.window_request_count,
  window_input_tokens = window_input_tokens + excluded.window_input_tokens,
  window_output_tokens = window_output_tokens + excluded.window_output_tokens,
  window_cached_tokens = window_cached_tokens + excluded.window_cached_tokens,
  window_image_input_tokens = window_image_input_tokens + excluded.window_image_input_tokens,
  window_image_output_tokens = window_image_output_tokens + excluded.window_image_output_tokens,
  window_image_request_count = window_image_request_count + excluded.window_image_request_count,
  window_image_request_failed_count = window_image_request_failed_count + excluded.window_image_request_failed_count,
  window_started_at = coalesce(account_usage.window_started_at, excluded.window_started_at),
  last_used_at = excluded.last_used_at";

pub(super) const RECORD_MODEL_USAGE_SQL: &str = r"
insert into account_model_usage (
  account_id,
  model,
  request_count,
  error_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  last_used_at
) values (?, ?, ?, ?, ?, ?, ?, ?)
on conflict(account_id, model) do update set
  request_count = request_count + excluded.request_count,
  error_count = error_count + excluded.error_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
  last_used_at = excluded.last_used_at";

pub(super) const LIST_MODEL_USAGE_SQL: &str = r"
select
  account_id,
  model,
  request_count,
  error_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  last_used_at
from account_model_usage
order by account_id asc, request_count desc, last_used_at desc, model asc";

pub(super) const LIST_QUOTA_SNAPSHOTS_SQL: &str = r"
select
  id,
  email,
  quota_json,
  quota_fetched_at
from accounts
where quota_json is not null
  and trim(quota_json) <> ''
order by coalesce(quota_fetched_at, '') desc, id desc";

pub(super) const UPDATE_QUOTA_JSON_SQL: &str = r"
update accounts
set
  quota_json = ?,
  quota_fetched_at = ?,
  plan_type = coalesce(?, plan_type),
  updated_at = ?
where id = ?";

pub(super) const APPLY_QUOTA_SNAPSHOT_SQL: &str = r"
update accounts
set
  quota_json = ?,
  quota_fetched_at = ?,
  plan_type = coalesce(?, plan_type),
  status = case
    when status in ('disabled', 'banned', 'expired', 'refreshing') then status
    when ? = 1 then 'quota_exhausted'
    when status = 'quota_exhausted' then 'active'
    else status
  end,
  quota_limit_reached = ?,
  quota_verify_required = 0,
  quota_cooldown_until = ?,
  updated_at = ?
where id = ?";

pub(super) const SELECT_RATE_LIMIT_WINDOW_SQL: &str = r"
select
  window_reset_at,
  limit_window_seconds
from account_usage
where account_id = ?";

pub(super) const SYNC_RATE_LIMIT_WINDOW_RESET_SQL: &str = r"
insert into account_usage (
  account_id,
  window_request_count,
  window_input_tokens,
  window_output_tokens,
  window_cached_tokens,
  window_image_input_tokens,
  window_image_output_tokens,
  window_image_request_count,
  window_image_request_failed_count,
  window_started_at,
  window_reset_at,
  limit_window_seconds
) values (?, 0, 0, 0, 0, 0, 0, 0, 0, ?, ?, ?)
on conflict(account_id) do update set
  window_request_count = 0,
  window_input_tokens = 0,
  window_output_tokens = 0,
  window_cached_tokens = 0,
  window_image_input_tokens = 0,
  window_image_output_tokens = 0,
  window_image_request_count = 0,
  window_image_request_failed_count = 0,
  window_started_at = excluded.window_started_at,
  window_reset_at = excluded.window_reset_at,
  limit_window_seconds = coalesce(excluded.limit_window_seconds, account_usage.limit_window_seconds)";

pub(super) const SYNC_RATE_LIMIT_WINDOW_SQL: &str = r"
insert into account_usage (
  account_id,
  window_reset_at,
  limit_window_seconds
) values (?, ?, ?)
on conflict(account_id) do update set
  window_reset_at = excluded.window_reset_at,
  limit_window_seconds = coalesce(excluded.limit_window_seconds, account_usage.limit_window_seconds)";

pub(super) const MARK_QUOTA_LIMITED_UNTIL_SQL: &str = r"
update accounts
set
  quota_limit_reached = 1,
  quota_verify_required = 0,
  quota_cooldown_until = ?,
  updated_at = ?
where id = ?";

pub(super) const SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL: &str = r"
update accounts
set
  cloudflare_cooldown_until = ?,
  updated_at = ?
where id = ?";

pub(super) const UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  plan_type = ?,
  access_token = ?,
  refresh_token = ?,
  access_token_expires_at = ?,
  next_refresh_at = ?,
  status = case
    when status in ('disabled', 'banned') then status
    else ?
  end,
  updated_at = ?
where id = ?";

pub(super) const UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  plan_type = ?,
  access_token = ?,
  access_token_expires_at = ?,
  next_refresh_at = ?,
  status = case
    when status in ('disabled', 'banned') then status
    else ?
  end,
  updated_at = ?
where id = ?";

pub(super) const SET_NEXT_REFRESH_AT_SQL: &str = r"
update accounts
set
  next_refresh_at = ?,
  updated_at = ?
where id = ?";

pub(super) const UPDATE_IMPORTED_ACCOUNT_WITH_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  label = ?,
  plan_type = ?,
  access_token = ?,
  refresh_token = ?,
  access_token_expires_at = ?,
  status = ?,
  quota_json = coalesce(?, quota_json),
  quota_fetched_at = case when ? is null then quota_fetched_at else ? end,
  quota_limit_reached = 0,
  quota_cooldown_until = null,
  quota_verify_required = ?,
  updated_at = ?
where id = ?";

pub(super) const UPDATE_IMPORTED_ACCOUNT_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  chatgpt_account_id = ?,
  chatgpt_user_id = ?,
  label = ?,
  plan_type = ?,
  access_token = ?,
  access_token_expires_at = ?,
  status = ?,
  quota_json = coalesce(?, quota_json),
  quota_fetched_at = case when ? is null then quota_fetched_at else ? end,
  quota_limit_reached = 0,
  quota_cooldown_until = null,
  quota_verify_required = ?,
  updated_at = ?
where id = ?";

pub(super) const APPLY_IMPORTED_QUOTA_STATE_SQL: &str = r"
update accounts
set
  quota_json = coalesce(?, quota_json),
  quota_fetched_at = case when ? is null then quota_fetched_at else ? end,
  plan_type = coalesce(?, plan_type),
  quota_limit_reached = 0,
  quota_cooldown_until = null,
  quota_verify_required = ?,
  updated_at = ?
where id = ?";

pub(super) const DELETE_ACCOUNT_SQL: &str = "delete from accounts where id = ?";
