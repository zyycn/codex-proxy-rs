use futures::TryStreamExt;
use sqlx::{Postgres, Row, SqlitePool, Transaction, types::Json};

use crate::infra::identity::hash_credential;

use super::{
    ImportSqliteError, ImportSqliteReport, parse_cookie_expiration, parse_json,
    parse_optional_json, parse_optional_timestamp, parse_timestamp,
};

pub(super) async fn import_core_tables(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    import_admin_users(source, target, report).await?;
    import_client_api_keys(source, target, report).await?;
    import_runtime_settings(source, target, report).await?;
    import_accounts(source, target, report).await?;
    import_account_usage(source, target, report).await?;
    import_account_cookies(source, target, report).await?;
    import_fingerprints(source, target, report).await?;
    import_fingerprint_history(source, target, report).await
}

async fn import_admin_users(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query(
        "select id, password_hash, created_at, updated_at from admin_users order by id",
    )
    .fetch(source);
    while let Some(row) = rows.try_next().await? {
        sqlx::query(
            "insert into admin_users (id, password_hash, created_at, updated_at)
             values ($1, $2, $3, $4)",
        )
        .bind(row.try_get::<String, _>("id")?)
        .bind(row.try_get::<String, _>("password_hash")?)
        .bind(parse_timestamp(
            "admin_users",
            "created_at",
            row.try_get("created_at")?,
        )?)
        .bind(parse_timestamp(
            "admin_users",
            "updated_at",
            row.try_get("updated_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("admin_users");
    }
    Ok(())
}

async fn import_client_api_keys(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query(
        "select id, name, prefix, key, label, enabled, created_at, last_used_at
         from client_api_keys order by id",
    )
    .fetch(source);
    while let Some(row) = rows.try_next().await? {
        sqlx::query(
            "insert into client_api_keys
             (id, name, prefix, key, label, enabled, created_at, last_used_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(row.try_get::<String, _>("id")?)
        .bind(row.try_get::<String, _>("name")?)
        .bind(row.try_get::<String, _>("prefix")?)
        .bind(row.try_get::<String, _>("key")?)
        .bind(row.try_get::<Option<String>, _>("label")?)
        .bind(row.try_get::<i64, _>("enabled")? == 1)
        .bind(parse_timestamp(
            "client_api_keys",
            "created_at",
            row.try_get("created_at")?,
        )?)
        .bind(parse_optional_timestamp(
            "client_api_keys",
            "last_used_at",
            row.try_get("last_used_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("client_api_keys");
    }
    Ok(())
}

async fn import_runtime_settings(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query(
        "select id, model_aliases_json, refresh_margin_seconds, refresh_concurrency,
                max_concurrent_per_account, request_interval_ms, rotation_strategy,
                admin_api_key, updated_at
         from runtime_settings order by id",
    )
    .fetch(source);
    while let Some(row) = rows.try_next().await? {
        let aliases: String = row.try_get("model_aliases_json")?;
        let admin_key = row
            .try_get::<Option<String>, _>("admin_api_key")?
            .and_then(|key| (!key.trim().is_empty()).then(|| hash_credential(&key)));
        sqlx::query(
            "insert into runtime_settings
             (id, model_aliases_json, refresh_margin_seconds, refresh_concurrency,
              max_concurrent_per_account, request_interval_ms, rotation_strategy,
              admin_api_key_hash, usage_retention_days, ops_error_retention_days,
              bucket_retention_days, updated_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, 30, 30, 90, $9)",
        )
        .bind(row.try_get::<i64, _>("id")?)
        .bind(Json(parse_json(
            "runtime_settings",
            "model_aliases_json",
            &aliases,
        )?))
        .bind(row.try_get::<i64, _>("refresh_margin_seconds")?)
        .bind(row.try_get::<i64, _>("refresh_concurrency")?)
        .bind(row.try_get::<i64, _>("max_concurrent_per_account")?)
        .bind(row.try_get::<i64, _>("request_interval_ms")?)
        .bind(row.try_get::<String, _>("rotation_strategy")?)
        .bind(admin_key)
        .bind(parse_timestamp(
            "runtime_settings",
            "updated_at",
            row.try_get("updated_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("runtime_settings");
    }
    Ok(())
}

async fn import_accounts(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query(
        "select id, email, chatgpt_account_id, chatgpt_user_id, label, plan_type,
                access_token, refresh_token, access_token_expires_at, next_refresh_at,
                status, quota_json, quota_fetched_at, quota_limit_reached,
                quota_verify_required, quota_cooldown_until, cloudflare_cooldown_until,
                added_at, updated_at
         from accounts order by id",
    )
    .fetch(source);
    while let Some(row) = rows.try_next().await? {
        let source_status = row.try_get::<String, _>("status")?;
        let (status, next_refresh_at) = if source_status == "refreshing" {
            report.add_normalized("accounts.refreshing_to_expired");
            ("expired", None)
        } else {
            (
                source_status.as_str(),
                parse_optional_timestamp(
                    "accounts",
                    "next_refresh_at",
                    row.try_get("next_refresh_at")?,
                )?,
            )
        };
        let quota = parse_optional_json(
            "accounts",
            "quota_json",
            row.try_get::<Option<String>, _>("quota_json")?,
        )?
        .map(Json);
        sqlx::query(
            "insert into accounts
             (id, email, chatgpt_account_id, chatgpt_user_id, label, plan_type,
              access_token, refresh_token, access_token_expires_at, next_refresh_at,
              status, quota_json, quota_fetched_at, quota_limit_reached,
              quota_verify_required, quota_cooldown_until, cloudflare_cooldown_until,
              added_at, updated_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                     $14, $15, $16, $17, $18, $19)",
        )
        .bind(row.try_get::<String, _>("id")?)
        .bind(row.try_get::<Option<String>, _>("email")?)
        .bind(row.try_get::<Option<String>, _>("chatgpt_account_id")?)
        .bind(row.try_get::<Option<String>, _>("chatgpt_user_id")?)
        .bind(row.try_get::<Option<String>, _>("label")?)
        .bind(row.try_get::<Option<String>, _>("plan_type")?)
        .bind(row.try_get::<String, _>("access_token")?)
        .bind(row.try_get::<Option<String>, _>("refresh_token")?)
        .bind(parse_optional_timestamp(
            "accounts",
            "access_token_expires_at",
            row.try_get("access_token_expires_at")?,
        )?)
        .bind(next_refresh_at)
        .bind(status)
        .bind(quota)
        .bind(parse_optional_timestamp(
            "accounts",
            "quota_fetched_at",
            row.try_get("quota_fetched_at")?,
        )?)
        .bind(row.try_get::<i64, _>("quota_limit_reached")? == 1)
        .bind(row.try_get::<i64, _>("quota_verify_required")? == 1)
        .bind(parse_optional_timestamp(
            "accounts",
            "quota_cooldown_until",
            row.try_get("quota_cooldown_until")?,
        )?)
        .bind(parse_optional_timestamp(
            "accounts",
            "cloudflare_cooldown_until",
            row.try_get("cloudflare_cooldown_until")?,
        )?)
        .bind(parse_timestamp(
            "accounts",
            "added_at",
            row.try_get("added_at")?,
        )?)
        .bind(parse_timestamp(
            "accounts",
            "updated_at",
            row.try_get("updated_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("accounts");
    }
    Ok(())
}

async fn import_account_usage(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query("select * from account_usage order by account_id").fetch(source);
    while let Some(row) = rows.try_next().await? {
        sqlx::query(
            "insert into account_usage
             (account_id, request_count, empty_response_count, input_tokens, output_tokens,
              cached_tokens, reasoning_tokens, total_tokens, image_input_tokens,
              image_output_tokens, image_request_count, image_request_failed_count,
              window_request_count, window_input_tokens, window_output_tokens,
              window_cached_tokens, window_image_input_tokens, window_image_output_tokens,
              window_image_request_count, window_image_request_failed_count,
              window_started_at, window_reset_at, limit_window_seconds, last_used_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24)",
        )
        .bind(row.try_get::<String, _>("account_id")?)
        .bind(row.try_get::<i64, _>("request_count")?)
        .bind(row.try_get::<i64, _>("empty_response_count")?)
        .bind(row.try_get::<i64, _>("input_tokens")?)
        .bind(row.try_get::<i64, _>("output_tokens")?)
        .bind(row.try_get::<i64, _>("cached_tokens")?)
        .bind(row.try_get::<i64, _>("reasoning_tokens")?)
        .bind(row.try_get::<i64, _>("total_tokens")?)
        .bind(row.try_get::<i64, _>("image_input_tokens")?)
        .bind(row.try_get::<i64, _>("image_output_tokens")?)
        .bind(row.try_get::<i64, _>("image_request_count")?)
        .bind(row.try_get::<i64, _>("image_request_failed_count")?)
        .bind(row.try_get::<i64, _>("window_request_count")?)
        .bind(row.try_get::<i64, _>("window_input_tokens")?)
        .bind(row.try_get::<i64, _>("window_output_tokens")?)
        .bind(row.try_get::<i64, _>("window_cached_tokens")?)
        .bind(row.try_get::<i64, _>("window_image_input_tokens")?)
        .bind(row.try_get::<i64, _>("window_image_output_tokens")?)
        .bind(row.try_get::<i64, _>("window_image_request_count")?)
        .bind(row.try_get::<i64, _>("window_image_request_failed_count")?)
        .bind(parse_optional_timestamp(
            "account_usage",
            "window_started_at",
            row.try_get("window_started_at")?,
        )?)
        .bind(parse_optional_timestamp(
            "account_usage",
            "window_reset_at",
            row.try_get("window_reset_at")?,
        )?)
        .bind(row.try_get::<Option<i64>, _>("limit_window_seconds")?)
        .bind(parse_optional_timestamp(
            "account_usage",
            "last_used_at",
            row.try_get("last_used_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("account_usage");
    }
    Ok(())
}

async fn import_account_cookies(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query(
        "select id, account_id, domain, name, value, path, expires_at, updated_at
         from account_cookies order by id",
    )
    .fetch(source);
    while let Some(row) = rows.try_next().await? {
        let expires_at = row
            .try_get::<Option<String>, _>("expires_at")?
            .and_then(|value| match parse_cookie_expiration(&value) {
                Some(timestamp) => Some(timestamp),
                None => {
                    report.add_cookie_expiration_parse_failure();
                    None
                }
            });
        sqlx::query(
            "insert into account_cookies
             (id, account_id, domain, name, value, path, expires_at, updated_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(row.try_get::<String, _>("id")?)
        .bind(row.try_get::<String, _>("account_id")?)
        .bind(row.try_get::<String, _>("domain")?)
        .bind(row.try_get::<String, _>("name")?)
        .bind(row.try_get::<String, _>("value")?)
        .bind(row.try_get::<String, _>("path")?)
        .bind(expires_at)
        .bind(parse_timestamp(
            "account_cookies",
            "updated_at",
            row.try_get("updated_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("account_cookies");
    }
    Ok(())
}

async fn import_fingerprints(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query("select * from fingerprints order by id").fetch(source);
    while let Some(row) = rows.try_next().await? {
        let default_headers: String = row.try_get("default_headers_json")?;
        let header_order: String = row.try_get("header_order_json")?;
        sqlx::query(
            "insert into fingerprints
             (id, originator, app_version, build_number, platform, arch, chromium_version,
              user_agent_template, default_headers_json, header_order_json, source,
              created_at, updated_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(row.try_get::<String, _>("id")?)
        .bind(row.try_get::<String, _>("originator")?)
        .bind(row.try_get::<String, _>("app_version")?)
        .bind(row.try_get::<String, _>("build_number")?)
        .bind(row.try_get::<String, _>("platform")?)
        .bind(row.try_get::<String, _>("arch")?)
        .bind(row.try_get::<String, _>("chromium_version")?)
        .bind(row.try_get::<String, _>("user_agent_template")?)
        .bind(Json(parse_json(
            "fingerprints",
            "default_headers_json",
            &default_headers,
        )?))
        .bind(Json(parse_json(
            "fingerprints",
            "header_order_json",
            &header_order,
        )?))
        .bind(row.try_get::<String, _>("source")?)
        .bind(parse_timestamp(
            "fingerprints",
            "created_at",
            row.try_get("created_at")?,
        )?)
        .bind(parse_timestamp(
            "fingerprints",
            "updated_at",
            row.try_get("updated_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("fingerprints");
    }
    Ok(())
}

async fn import_fingerprint_history(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows =
        sqlx::query("select * from fingerprint_update_history order by id").fetch(source);
    while let Some(row) = rows.try_next().await? {
        let manifest = parse_optional_json(
            "fingerprint_update_history",
            "manifest_json",
            row.try_get::<Option<String>, _>("manifest_json")?,
        )?
        .map(Json);
        sqlx::query(
            "insert into fingerprint_update_history
             (id, current_fingerprint_id, app_version, build_number, chromium_version,
              source, manifest_json, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(row.try_get::<String, _>("id")?)
        .bind(row.try_get::<String, _>("current_fingerprint_id")?)
        .bind(row.try_get::<String, _>("app_version")?)
        .bind(row.try_get::<String, _>("build_number")?)
        .bind(row.try_get::<Option<String>, _>("chromium_version")?)
        .bind(row.try_get::<String, _>("source")?)
        .bind(manifest)
        .bind(parse_timestamp(
            "fingerprint_update_history",
            "created_at",
            row.try_get("created_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("fingerprint_update_history");
    }
    Ok(())
}

pub(super) async fn count_discarded_runtime_tables(
    source: &SqlitePool,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let (sessions, affinities, leases, snapshots, model_usage): (i64, i64, i64, i64, i64) =
        sqlx::query_as(
            "select
           (select count(*) from admin_sessions),
           (select count(*) from session_affinities),
           (select count(*) from account_refresh_leases),
           (select count(*) from model_plan_snapshots),
           (select count(*) from account_model_usage)",
        )
        .fetch_one(source)
        .await?;
    report.add_discarded("admin_sessions", sessions.max(0) as u64);
    report.add_discarded("session_affinities", affinities.max(0) as u64);
    report.add_discarded("account_refresh_leases", leases.max(0) as u64);
    report.add_discarded("model_plan_snapshots", snapshots.max(0) as u64);
    report.add_discarded("account_model_usage", model_usage.max(0) as u64);
    Ok(())
}
