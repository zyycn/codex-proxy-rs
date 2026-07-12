use futures::TryStreamExt;
use serde_json::Value;
use sqlx::{Postgres, Row, SqlitePool, Transaction, types::Json};

use super::{
    ImportSqliteError, ImportSqliteReport, normalized_dimension, parse_json, parse_timestamp,
};

pub(super) async fn import_telemetry_tables(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    import_mixed_usage_records(source, target, report).await?;
    import_ops_error_logs(source, target, report).await?;
    import_request_time_buckets(source, target, report).await
}

async fn import_mixed_usage_records(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows = sqlx::query(
        "select id, request_id, kind, level, account_id, route, model, status_code,
                transport, attempt_index, upstream_status_code, failure_class,
                response_id, upstream_request_id, latency_ms, message, metadata_json,
                created_at
         from usage_records order by created_at, id",
    )
    .fetch(source);

    while let Some(row) = rows.try_next().await? {
        let level: String = row.try_get("level")?;
        let account_id: Option<String> = row.try_get("account_id")?;
        let model: Option<String> = row.try_get("model")?;
        let status_code: Option<i64> = row.try_get("status_code")?;
        let metadata_text: String = row.try_get("metadata_json")?;
        let metadata = parse_json("usage_records", "metadata_json", &metadata_text)?;

        if level == "error" {
            insert_usage_error(target, &row, metadata).await?;
            report.add_imported("ops_error_logs");
            continue;
        }

        let valid_account = account_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        let valid_model = model
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        let valid_status = status_code.is_some_and(|status| (200..=399).contains(&status));
        if !valid_account || !valid_model || !valid_status {
            report.add_discarded("usage_record_noise", 1);
            continue;
        }

        insert_success_record(
            target,
            &row,
            account_id.expect("validated account id"),
            model.expect("validated model"),
            status_code.expect("validated status code"),
            metadata,
        )
        .await?;
        report.add_imported("usage_records");
    }
    Ok(())
}

async fn insert_success_record(
    target: &mut Transaction<'_, Postgres>,
    row: &sqlx::sqlite::SqliteRow,
    account_id: String,
    model: String,
    status_code: i64,
    mut metadata: Value,
) -> Result<(), ImportSqliteError> {
    let requested_model = metadata_string(&metadata, &["requestedModel", "requested_model"]);
    let upstream_model = metadata_string(&metadata, &["upstreamModel", "upstream_model"]);
    let service_tier = metadata_string(&metadata, &["serviceTier", "service_tier"]);
    let first_token_ms = metadata_nonnegative(&metadata, &["firstTokenMs", "first_token_ms"]);
    let input_tokens = usage_number(&metadata, &["inputTokens", "input_tokens"]);
    let output_tokens = usage_number(&metadata, &["outputTokens", "output_tokens"]);
    let cached_tokens = usage_number(&metadata, &["cachedTokens", "cached_tokens"]);
    let reasoning_tokens = usage_number(&metadata, &["reasoningTokens", "reasoning_tokens"]);
    strip_promoted_success_fields(&mut metadata);

    sqlx::query(
        "insert into usage_records
         (id, request_id, client_api_key_id, kind, route, provider, account_id, model,
          requested_model, upstream_model, service_tier, status_code, transport,
          attempt_index, response_id, upstream_request_id, latency_ms, first_token_ms,
          input_tokens, output_tokens, cached_tokens, reasoning_tokens, message,
          metadata_json, created_at)
         values ($1, $2, null, $3, $4, 'openai', $5, $6, $7, $8, $9, $10,
                 $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23)",
    )
    .bind(row.try_get::<String, _>("id")?)
    .bind(row.try_get::<Option<String>, _>("request_id")?)
    .bind(row.try_get::<String, _>("kind")?)
    .bind(row.try_get::<Option<String>, _>("route")?)
    .bind(account_id)
    .bind(model)
    .bind(requested_model)
    .bind(upstream_model)
    .bind(service_tier)
    .bind(status_code as i32)
    .bind(row.try_get::<Option<String>, _>("transport")?)
    .bind(row.try_get::<Option<i64>, _>("attempt_index")?)
    .bind(row.try_get::<Option<String>, _>("response_id")?)
    .bind(row.try_get::<Option<String>, _>("upstream_request_id")?)
    .bind(row.try_get::<Option<i64>, _>("latency_ms")?)
    .bind(first_token_ms)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cached_tokens)
    .bind(reasoning_tokens)
    .bind(row.try_get::<String, _>("message")?)
    .bind(Json(metadata))
    .bind(parse_timestamp(
        "usage_records",
        "created_at",
        row.try_get("created_at")?,
    )?)
    .execute(&mut **target)
    .await?;
    Ok(())
}

async fn insert_usage_error(
    target: &mut Transaction<'_, Postgres>,
    row: &sqlx::sqlite::SqliteRow,
    metadata: Value,
) -> Result<(), ImportSqliteError> {
    sqlx::query(
        "insert into ops_error_logs
         (id, request_id, client_api_key_id, kind, provider, account_id, route, model,
          status_code, client_status_code, upstream_status_code, transport, attempt_index,
          failure_class, response_id, upstream_request_id, latency_ms, message,
          metadata_json, created_at)
         values ($1, $2, null, $3, 'openai', $4, $5, $6, $7, null, $8, $9,
                 $10, $11, $12, $13, $14, $15, $16, $17)",
    )
    .bind(row.try_get::<String, _>("id")?)
    .bind(row.try_get::<Option<String>, _>("request_id")?)
    .bind(row.try_get::<String, _>("kind")?)
    .bind(row.try_get::<Option<String>, _>("account_id")?)
    .bind(row.try_get::<Option<String>, _>("route")?)
    .bind(row.try_get::<Option<String>, _>("model")?)
    .bind(optional_status(row.try_get("status_code")?))
    .bind(optional_status(row.try_get("upstream_status_code")?))
    .bind(row.try_get::<Option<String>, _>("transport")?)
    .bind(row.try_get::<Option<i64>, _>("attempt_index")?)
    .bind(row.try_get::<Option<String>, _>("failure_class")?)
    .bind(row.try_get::<Option<String>, _>("response_id")?)
    .bind(row.try_get::<Option<String>, _>("upstream_request_id")?)
    .bind(row.try_get::<Option<i64>, _>("latency_ms")?)
    .bind(row.try_get::<String, _>("message")?)
    .bind(Json(metadata))
    .bind(parse_timestamp(
        "usage_records",
        "created_at",
        row.try_get("created_at")?,
    )?)
    .execute(&mut **target)
    .await?;
    Ok(())
}

async fn import_ops_error_logs(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows =
        sqlx::query("select * from ops_error_logs order by created_at, id").fetch(source);
    while let Some(row) = rows.try_next().await? {
        let metadata_text: String = row.try_get("metadata_json")?;
        sqlx::query(
            "insert into ops_error_logs
             (id, request_id, client_api_key_id, kind, provider, account_id, route, model,
              status_code, client_status_code, upstream_status_code, transport, attempt_index,
              failure_class, response_id, upstream_request_id, latency_ms, message,
              metadata_json, created_at)
             values ($1, $2, null, $3, 'openai', $4, $5, $6, $7, $8, $9, $10,
                     $11, $12, $13, $14, $15, $16, $17, $18)",
        )
        .bind(row.try_get::<String, _>("id")?)
        .bind(row.try_get::<Option<String>, _>("request_id")?)
        .bind(row.try_get::<String, _>("kind")?)
        .bind(row.try_get::<Option<String>, _>("account_id")?)
        .bind(row.try_get::<Option<String>, _>("route")?)
        .bind(row.try_get::<Option<String>, _>("model")?)
        .bind(optional_status(row.try_get("status_code")?))
        .bind(optional_status(row.try_get("client_status_code")?))
        .bind(optional_status(row.try_get("upstream_status_code")?))
        .bind(row.try_get::<Option<String>, _>("transport")?)
        .bind(row.try_get::<Option<i64>, _>("attempt_index")?)
        .bind(row.try_get::<Option<String>, _>("failure_class")?)
        .bind(row.try_get::<Option<String>, _>("response_id")?)
        .bind(row.try_get::<Option<String>, _>("upstream_request_id")?)
        .bind(row.try_get::<Option<i64>, _>("latency_ms")?)
        .bind(row.try_get::<String, _>("message")?)
        .bind(Json(parse_json(
            "ops_error_logs",
            "metadata_json",
            &metadata_text,
        )?))
        .bind(parse_timestamp(
            "ops_error_logs",
            "created_at",
            row.try_get("created_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("ops_error_logs");
    }
    Ok(())
}

async fn import_request_time_buckets(
    source: &SqlitePool,
    target: &mut Transaction<'_, Postgres>,
    report: &mut ImportSqliteReport,
) -> Result<(), ImportSqliteError> {
    let mut rows =
        sqlx::query("select * from usage_time_buckets order by bucket_start").fetch(source);
    while let Some(row) = rows.try_next().await? {
        let request_count: i64 = row.try_get("request_count")?;
        let error_count: i64 = row.try_get("error_count")?;
        let min_latency_ms: i64 = row.try_get("min_latency_ms")?;
        sqlx::query(
            "insert into request_time_buckets
             (bucket_start, provider, account_id, model, service_tier, success_count,
              error_count, input_tokens, output_tokens, cached_tokens,
              first_token_latency_sum, first_token_latency_count, latency_sum,
              latency_count, max_latency_ms, min_latency_ms, updated_at)
             values ($1, 'openai', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                     $12, $13, $14, $15, $16)",
        )
        .bind(parse_timestamp(
            "usage_time_buckets",
            "bucket_start",
            row.try_get("bucket_start")?,
        )?)
        .bind(normalized_dimension(row.try_get("account_id")?))
        .bind(normalized_dimension(row.try_get("model")?))
        .bind(normalized_dimension(row.try_get("service_tier")?))
        .bind(request_count.saturating_sub(error_count).max(0))
        .bind(error_count)
        .bind(row.try_get::<i64, _>("input_tokens")?)
        .bind(row.try_get::<i64, _>("output_tokens")?)
        .bind(row.try_get::<i64, _>("cached_tokens")?)
        .bind(row.try_get::<i64, _>("first_token_latency_sum")?)
        .bind(row.try_get::<i64, _>("first_token_latency_count")?)
        .bind(row.try_get::<i64, _>("latency_sum")?)
        .bind(row.try_get::<i64, _>("latency_count")?)
        .bind(row.try_get::<i64, _>("max_latency_ms")?)
        .bind((min_latency_ms != 0).then_some(min_latency_ms))
        .bind(parse_timestamp(
            "usage_time_buckets",
            "updated_at",
            row.try_get("updated_at")?,
        )?)
        .execute(&mut **target)
        .await?;
        report.add_imported("request_time_buckets");
    }
    Ok(())
}

fn optional_status(status: Option<i64>) -> Option<i32> {
    status.and_then(|status| i32::try_from(status).ok())
}

fn metadata_string(metadata: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn metadata_nonnegative(metadata: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_i64))
        .filter(|value| *value >= 0)
}

fn usage_number(metadata: &Value, keys: &[&str]) -> Option<i64> {
    metadata
        .get("usage")
        .and_then(|usage| metadata_nonnegative(usage, keys))
        .or_else(|| metadata_nonnegative(metadata, keys))
}

fn strip_promoted_success_fields(metadata: &mut Value) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    for key in [
        "usage",
        "route",
        "apiKind",
        "requestedModel",
        "requested_model",
        "upstreamModel",
        "upstream_model",
        "serviceTier",
        "service_tier",
        "statusCode",
        "transport",
        "attemptIndex",
        "responseId",
        "upstreamRequestId",
        "openaiRequestId",
        "firstTokenMs",
        "first_token_ms",
    ] {
        object.remove(key);
    }
}
