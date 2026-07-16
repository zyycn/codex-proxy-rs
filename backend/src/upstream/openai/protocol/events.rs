use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::upstream::openai::protocol::sse::{SseError, parse_sse_events};

/// 从 Codex/OpenAI usage 结构中提取出的标准化 token 用量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// 输入 token 数。
    pub input_tokens: u64,
    /// 输出 token 数。
    pub output_tokens: u64,
    /// 命中缓存的输入 token 数。
    pub cached_tokens: u64,
    /// 写入 prompt cache 的输入 token 数。
    pub cache_write_tokens: u64,
    /// 输出 token 中的 reasoning token 数。
    pub reasoning_tokens: u64,
    /// 图片工具输入 token 数。
    pub image_input_tokens: u64,
    /// 图片工具输出 token 数。
    pub image_output_tokens: u64,
    /// 主模型总 token 数，不包含图片工具 token。
    pub total_tokens: u64,
}

/// 从单个 JSON 响应体中提取用量。
///
/// 该函数同时支持 Codex usage 结构和 OpenAI usage 结构。
pub fn extract_usage(body: &Value) -> Option<TokenUsage> {
    let usage = body.get("usage").unwrap_or(body);
    if !usage.is_object() {
        return None;
    }

    let input_tokens = number_field(usage, "input_tokens")
        .or_else(|| number_field(usage, "prompt_tokens"))
        .unwrap_or_default();
    let output_tokens = number_field(usage, "output_tokens")
        .or_else(|| number_field(usage, "completion_tokens"))
        .unwrap_or_default();
    let cached_tokens = nested_number_field(usage, &["input_tokens_details", "cached_tokens"])
        .or_else(|| nested_number_field(usage, &["prompt_tokens_details", "cached_tokens"]))
        .or_else(|| number_field(usage, "cached_tokens"))
        .unwrap_or_default();
    let cache_write_tokens =
        nested_number_field(usage, &["input_tokens_details", "cache_write_tokens"])
            .or_else(|| {
                nested_number_field(usage, &["prompt_tokens_details", "cache_write_tokens"])
            })
            .or_else(|| number_field(usage, "cache_write_tokens"))
            .unwrap_or_default();
    let reasoning_tokens =
        nested_number_field(usage, &["output_tokens_details", "reasoning_tokens"])
            .or_else(|| {
                nested_number_field(usage, &["completion_tokens_details", "reasoning_tokens"])
            })
            .or_else(|| number_field(usage, "reasoning_tokens"))
            .unwrap_or_default();
    let image_input_tokens =
        nested_number_field(body, &["tool_usage", "image_gen", "input_tokens"]).unwrap_or_default();
    let image_output_tokens =
        nested_number_field(body, &["tool_usage", "image_gen", "output_tokens"])
            .unwrap_or_default();
    let total_tokens = number_field(usage, "total_tokens").unwrap_or(input_tokens + output_tokens);

    let has_usage = [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "cached_tokens",
        "cache_write_tokens",
        "reasoning_tokens",
        "total_tokens",
    ]
    .iter()
    .any(|field| usage.get(*field).is_some())
        || usage.get("input_tokens_details").is_some()
        || usage.get("prompt_tokens_details").is_some()
        || usage.get("output_tokens_details").is_some()
        || usage.get("completion_tokens_details").is_some()
        || image_input_tokens > 0
        || image_output_tokens > 0;

    has_usage.then_some(TokenUsage {
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_write_tokens,
        reasoning_tokens,
        image_input_tokens,
        image_output_tokens,
        total_tokens,
    })
}

/// 从完整 SSE 文本中提取最终可见用量。
///
/// 如果存在 `response.completed` 的 usage，则优先返回它；否则回退到最后一条
/// 可见 usage 事件。
///
/// # Errors
///
/// 当输入不是合法 SSE 流时，返回 [`SseError`]。
pub fn extract_sse_usage(body: &str) -> Result<Option<TokenUsage>, SseError> {
    let events = parse_sse_events(body)?;
    let mut fallback_usage: Option<TokenUsage> = None;

    for event in events {
        if event.data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        let event_usage = value
            .get("response")
            .and_then(extract_usage)
            .or_else(|| extract_usage(&value));
        if let Some(event_usage) = event_usage {
            if event.event.as_deref() == Some("response.completed") {
                return Ok(Some(event_usage));
            }
            fallback_usage = Some(event_usage);
        }
    }

    Ok(fallback_usage)
}

/// 从上游错误响应体中提取 retry-after 秒数。
///
/// 支持结构化的 `resets_in_seconds` / `resets_at`，也支持官方错误消息里的
/// `try again in 11.054s` / `try again in 28ms` 文本。
pub fn retry_after_seconds_from_body(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    retry_after_seconds_from_value(&value)
}

/// 从已解析的上游错误事件中提取重试秒数，不做业务错误分类。
pub(crate) fn retry_after_seconds_from_value(value: &Value) -> Option<u64> {
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .unwrap_or(value);
    retry_after_seconds_field(error)
        .or_else(|| {
            error
                .get("resets_in_seconds")
                .and_then(Value::as_u64)
                .filter(|seconds| *seconds > 0)
        })
        .or_else(|| retry_after_seconds_from_resets_at(error))
        .or_else(|| retry_after_seconds_field(value))
        .or_else(|| retry_after_seconds_header(value))
        .or_else(|| retry_after_seconds_from_rate_limit_message(error))
}

fn retry_after_seconds_field(value: &Value) -> Option<u64> {
    value
        .get("retry_after_seconds")
        .or_else(|| value.get("retry_after"))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
        })
        .filter(|seconds| *seconds > 0)
}

fn retry_after_seconds_header(value: &Value) -> Option<u64> {
    value
        .get("headers")
        .and_then(Value::as_object)
        .and_then(|headers| {
            headers.iter().find_map(|(name, value)| {
                if name.eq_ignore_ascii_case("retry-after") {
                    json_value_as_positive_u64(value)
                } else {
                    None
                }
            })
        })
}

fn json_value_as_positive_u64(value: &Value) -> Option<u64> {
    let seconds = match value {
        Value::Number(value) => value.as_u64()?,
        Value::String(value) => value.trim().parse::<u64>().ok()?,
        Value::Array(values) => values.first().and_then(json_value_as_positive_u64)?,
        Value::Null | Value::Bool(_) | Value::Object(_) => return None,
    };
    (seconds > 0).then_some(seconds)
}

/// 标准化的单个限流窗口。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimitWindow {
    /// 已使用百分比。
    pub used_percent: f64,
    /// 窗口分钟数。
    pub window_minutes: Option<u64>,
    /// 重置时间戳。
    pub reset_at: Option<i64>,
}

/// 单个计量项的限流信息。
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitDetails {
    /// 稳定的计量项 ID。
    pub limit_id: String,
    /// 上游提供的可读名称。
    pub limit_name: Option<String>,
    /// 当前请求是否被允许。
    pub allowed: Option<bool>,
    /// 当前窗口是否已经触顶。
    pub limit_reached: Option<bool>,
    /// 主限流窗口。
    pub primary: Option<RateLimitWindow>,
    /// 次级限流窗口。
    pub secondary: Option<RateLimitWindow>,
}

/// 上游账户的 credits 快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsSnapshot {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

/// 从 header 或内部事件中解析出的完整限流状态。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRateLimits {
    /// 以标准化 limit ID 为键的全部计量项。
    pub limits: BTreeMap<String, RateLimitDetails>,
    /// 当前响应对应的计量项 ID。
    pub active_limit: Option<String>,
    pub credits: Option<CreditsSnapshot>,
    pub plan_type: Option<String>,
    pub promo_message: Option<String>,
    pub rate_limit_reached_type: Option<String>,
}

/// 从响应头对中解析限流信息。
pub fn parse_rate_limit_headers(headers: &[(String, String)]) -> Option<ParsedRateLimits> {
    let mut normalized = BTreeMap::new();
    for (name, value) in headers {
        normalized.insert(name.to_ascii_lowercase(), value.trim());
    }

    let active_limit = lookup_non_empty(&normalized, "x-codex-active-limit")
        .map(|limit_id| normalize_limit_id(&limit_id));
    let mut limit_ids = normalized
        .keys()
        .filter_map(|name| rate_limit_id_from_header_name(name))
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(active_limit) = &active_limit {
        limit_ids.insert(active_limit.clone());
    }
    limit_ids.insert("codex".to_string());
    let limits = limit_ids
        .into_iter()
        .filter_map(|limit_id| {
            let prefix = format!("x-{}", limit_id.replace('_', "-"));
            parse_details_from_lookup(&normalized, &prefix, &limit_id)
                .map(|details| (limit_id, details))
        })
        .collect::<BTreeMap<_, _>>();
    let credits = parse_credits_from_lookup(&normalized);
    let plan_type = lookup_non_empty(&normalized, "x-codex-plan-type");
    let promo_message = lookup_non_empty(&normalized, "x-codex-promo-message");
    let rate_limit_reached_type = lookup_non_empty(&normalized, "x-codex-rate-limit-reached-type");

    if limits.is_empty()
        && active_limit.is_none()
        && credits.is_none()
        && plan_type.is_none()
        && promo_message.is_none()
        && rate_limit_reached_type.is_none()
    {
        return None;
    }

    Some(ParsedRateLimits {
        limits,
        active_limit,
        credits,
        plan_type,
        promo_message,
        rate_limit_reached_type,
    })
}

/// 判断响应头是否属于可被动同步的限流领域。
pub(crate) fn is_rate_limit_header_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    normalized == "retry-after"
        || normalized.contains("ratelimit")
        || normalized.contains("rate-limit")
        || matches!(
            normalized.as_str(),
            "x-codex-credits-has-credits"
                | "x-codex-credits-unlimited"
                | "x-codex-credits-balance"
                | "x-codex-active-limit"
                | "x-codex-plan-type"
                | "x-codex-promo-message"
                | "x-codex-rate-limit-reached-type"
        )
        || rate_limit_id_from_header_name(&normalized).is_some()
}

/// 从内部 `codex.rate_limits` 事件中解析限流信息。
pub fn parse_rate_limits_event(value: &Value) -> Option<ParsedRateLimits> {
    if value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|event_type| event_type != "codex.rate_limits")
    {
        return None;
    }

    let limit_id = rate_limit_name(value)
        .map(normalize_limit_id)
        .unwrap_or_else(|| "codex".to_string());
    let limit_name = value
        .get("metered_limit_name")
        .and_then(Value::as_str)
        .and_then(|_| value.get("limit_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string);
    let details = value
        .get("rate_limits")
        .and_then(|details| parse_details_from_object(details, &limit_id, limit_name));
    let credits = value.get("credits").and_then(parse_credits_from_object);
    let plan_type = value
        .get("plan_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
        .map(ToString::to_string);

    if details.is_none() && credits.is_none() && plan_type.is_none() {
        return None;
    }

    let mut limits = BTreeMap::new();
    if let Some(details) = details {
        limits.insert(limit_id.clone(), details);
    }
    Some(ParsedRateLimits {
        limits,
        active_limit: Some(limit_id),
        credits,
        plan_type,
        promo_message: value
            .get("promo_message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(ToString::to_string),
        rate_limit_reached_type: value
            .get("rate_limit_reached_type")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .map(ToString::to_string),
    })
}

/// 从原始 JSON 文本中解析内部 `codex.rate_limits` 事件。
pub fn parse_rate_limits_event_raw(raw: &str) -> Option<ParsedRateLimits> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| parse_rate_limits_event(&value))
}

/// 将解析后的限流状态转换为配额响应体。
pub fn rate_limit_quota(
    rate_limits: &ParsedRateLimits,
    plan_type: Option<&str>,
    existing_quota: Option<&Value>,
) -> Value {
    let mut snapshots = rate_limits
        .limits
        .values()
        .filter_map(|details| {
            let (source, metered_feature) = quota_identity(&details.limit_id);
            let limit_name = details
                .limit_name
                .as_deref()
                .or_else(|| (source != "core").then_some(details.limit_id.as_str()));
            quota_snapshot_from_windows(
                source,
                limit_name,
                metered_feature,
                details.primary,
                details.secondary,
                details.allowed,
                details.limit_reached,
            )
        })
        .collect::<Vec<_>>();

    if let Some(existing_quota) = existing_quota
        && let Some(existing_snapshots) = existing_quota.get("snapshots").and_then(Value::as_array)
    {
        for snapshot in existing_snapshots {
            if !rate_limits
                .limits
                .keys()
                .any(|limit_id| quota_snapshot_matches_limit(snapshot, limit_id))
            {
                snapshots.push(snapshot.clone());
            }
        }
    }

    let monthly_limit = monthly_limit_from_snapshots(&snapshots)
        .or_else(|| existing_quota.and_then(|quota| quota.get("monthly_limit").cloned()))
        .unwrap_or(Value::Null);
    let credits = rate_limits
        .credits
        .as_ref()
        .map(|credits| {
            json!({
                "has_credits": credits.has_credits,
                "unlimited": credits.unlimited,
                "balance": credits.balance,
            })
        })
        .or_else(|| existing_quota.and_then(|quota| quota.get("credits").cloned()))
        .unwrap_or(Value::Null);
    let spend_control = existing_quota
        .and_then(|quota| quota.get("spend_control").cloned())
        .unwrap_or(Value::Null);
    let active_limit = rate_limits.active_limit.as_ref().map_or_else(
        || {
            existing_quota
                .and_then(|quota| quota.get("active_limit"))
                .cloned()
                .unwrap_or(Value::Null)
        },
        |limit| Value::String(limit.clone()),
    );

    json!({
        "plan_type": rate_limits.plan_type.as_deref().or(plan_type).unwrap_or("unknown"),
        "active_limit": active_limit,
        "snapshots": snapshots,
        "monthly_limit": monthly_limit,
        "credits": credits,
        "spend_control": spend_control,
        "promo_message": rate_limits.promo_message,
        "rate_limit_reached_type": rate_limits.rate_limit_reached_type,
    })
}

/// 将限流状态转换回内部传输头键值对。
pub fn rate_limits_to_header_pairs(rate_limits: &ParsedRateLimits) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(active_limit) = &rate_limits.active_limit {
        headers.push(("x-codex-active-limit".to_string(), active_limit.clone()));
    }
    for details in rate_limits.limits.values() {
        let prefix = format!("x-{}", details.limit_id.replace('_', "-"));
        push_window_headers(&mut headers, &format!("{prefix}-primary"), details.primary);
        push_window_headers(
            &mut headers,
            &format!("{prefix}-secondary"),
            details.secondary,
        );
        if let Some(limit_name) = &details.limit_name {
            headers.push((format!("{prefix}-limit-name"), limit_name.clone()));
        }
    }
    if let Some(credits) = &rate_limits.credits {
        headers.push((
            "x-codex-credits-has-credits".to_string(),
            credits.has_credits.to_string(),
        ));
        headers.push((
            "x-codex-credits-unlimited".to_string(),
            credits.unlimited.to_string(),
        ));
        if let Some(balance) = &credits.balance {
            headers.push(("x-codex-credits-balance".to_string(), balance.clone()));
        }
    }
    if let Some(plan_type) = &rate_limits.plan_type {
        headers.push(("x-codex-plan-type".to_string(), plan_type.clone()));
    }
    if let Some(promo_message) = &rate_limits.promo_message {
        headers.push(("x-codex-promo-message".to_string(), promo_message.clone()));
    }
    if let Some(reached_type) = &rate_limits.rate_limit_reached_type {
        headers.push((
            "x-codex-rate-limit-reached-type".to_string(),
            reached_type.clone(),
        ));
    }
    headers
}

fn quota_identity(limit_id: &str) -> (&'static str, Option<&str>) {
    if limit_id == "codex" {
        ("core", None)
    } else if is_review_limit_name(Some(limit_id)) {
        ("code_review", Some(limit_id))
    } else {
        ("additional", Some(limit_id))
    }
}

fn quota_snapshot_matches_limit(snapshot: &Value, limit_id: &str) -> bool {
    let (source, metered_feature) = quota_identity(limit_id);
    if snapshot.get("source").and_then(Value::as_str) != Some(source) {
        return false;
    }
    source == "core"
        || snapshot.get("metered_feature").and_then(Value::as_str) == metered_feature
        || snapshot.get("limit_name").and_then(Value::as_str) == Some(limit_id)
}

fn number_field(value: &Value, field: &str) -> Option<u64> {
    value.get(field)?.as_u64()
}

fn nested_number_field(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_u64()
}

fn retry_after_seconds_from_resets_at(error: &Value) -> Option<u64> {
    let resets_at = error.get("resets_at").and_then(Value::as_u64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (resets_at > now).then_some(resets_at - now)
}

fn retry_after_seconds_from_rate_limit_message(error: &Value) -> Option<u64> {
    let code = error
        .get("code")
        .or_else(|| error.get("type"))
        .and_then(Value::as_str)?;
    if !matches!(
        code,
        "rate_limit_exceeded"
            | "rate_limit_reached"
            | "usage_limit_reached"
            | "workspace_owner_usage_limit_reached"
            | "workspace_member_usage_limit_reached"
    ) {
        return None;
    }
    let message = error.get("message").and_then(Value::as_str)?;
    parse_try_again_delay_seconds(message)
}

fn parse_try_again_delay_seconds(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let marker = "try again in";
    let offset = lower.find(marker)? + marker.len();
    let remainder = message.get(offset..)?.trim_start();
    let number_end = number_prefix_len(remainder)?;
    let value = remainder.get(..number_end)?.parse::<f64>().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let unit_text = remainder
        .get(number_end..)?
        .trim_start()
        .to_ascii_lowercase();
    let unit = unit_token(&unit_text)?;
    match unit {
        "ms" => positive_seconds_ceil(value / 1000.0),
        "s" | "second" | "seconds" => positive_seconds_ceil(value),
        _ => None,
    }
}

fn number_prefix_len(input: &str) -> Option<usize> {
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut end = 0;
    for (index, ch) in input.char_indices() {
        if ch.is_ascii_digit() {
            seen_digit = true;
            end = index + ch.len_utf8();
        } else if ch == '.' && !seen_dot {
            seen_dot = true;
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    seen_digit.then_some(end)
}

fn unit_token(input: &str) -> Option<&str> {
    let end = input
        .char_indices()
        .find_map(|(index, ch)| (!ch.is_ascii_alphabetic()).then_some(index))
        .unwrap_or(input.len());
    (end > 0).then_some(&input[..end])
}

fn positive_seconds_ceil(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds.ceil())
        .ok()
        .map(|duration| duration.as_secs())
}

fn push_window_headers(
    headers: &mut Vec<(String, String)>,
    prefix: &str,
    window: Option<RateLimitWindow>,
) {
    let Some(window) = window else {
        return;
    };
    headers.push((
        format!("{prefix}-used-percent"),
        window.used_percent.to_string(),
    ));
    if let Some(window_minutes) = window.window_minutes {
        headers.push((
            format!("{prefix}-window-minutes"),
            window_minutes.to_string(),
        ));
    }
    if let Some(reset_at) = window.reset_at {
        headers.push((format!("{prefix}-reset-at"), reset_at.to_string()));
    }
}

fn parse_details_from_lookup(
    headers: &BTreeMap<String, &str>,
    prefix: &str,
    limit_id: &str,
) -> Option<RateLimitDetails> {
    let primary = parse_window_from_lookup(headers, &format!("{prefix}-primary"));
    let secondary = parse_window_from_lookup(headers, &format!("{prefix}-secondary"));
    let limit_name = lookup_non_empty(headers, &format!("{prefix}-limit-name"));
    let allowed = lookup_bool(headers, &format!("{prefix}-allowed"));
    let limit_reached = lookup_bool(headers, &format!("{prefix}-limit-reached"));
    if primary.is_none()
        && secondary.is_none()
        && limit_name.is_none()
        && allowed.is_none()
        && limit_reached.is_none()
    {
        return None;
    }
    Some(RateLimitDetails {
        limit_id: limit_id.to_string(),
        limit_name,
        allowed,
        limit_reached,
        primary,
        secondary,
    })
}

fn rate_limit_id_from_header_name(name: &str) -> Option<String> {
    const SUFFIXES: [&str; 9] = [
        "-primary-used-percent",
        "-primary-window-minutes",
        "-primary-reset-at",
        "-secondary-used-percent",
        "-secondary-window-minutes",
        "-secondary-reset-at",
        "-limit-name",
        "-allowed",
        "-limit-reached",
    ];
    let name = name.strip_prefix("x-")?;
    let raw_id = SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))?;
    (!raw_id.is_empty()).then(|| normalize_limit_id(raw_id))
}

fn parse_credits_from_lookup(headers: &BTreeMap<String, &str>) -> Option<CreditsSnapshot> {
    Some(CreditsSnapshot {
        has_credits: lookup_bool(headers, "x-codex-credits-has-credits")?,
        unlimited: lookup_bool(headers, "x-codex-credits-unlimited")?,
        balance: lookup_non_empty(headers, "x-codex-credits-balance"),
    })
}

fn parse_credits_from_object(value: &Value) -> Option<CreditsSnapshot> {
    Some(CreditsSnapshot {
        has_credits: value.get("has_credits")?.as_bool()?,
        unlimited: value.get("unlimited")?.as_bool()?,
        balance: value.get("balance").and_then(|balance| match balance {
            Value::String(balance) => Some(balance.clone()),
            Value::Number(balance) => Some(balance.to_string()),
            _ => None,
        }),
    })
}

fn lookup_non_empty(headers: &BTreeMap<String, &str>, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn lookup_bool(headers: &BTreeMap<String, &str>, name: &str) -> Option<bool> {
    match headers.get(name)?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        value if value.eq_ignore_ascii_case("true") => Some(true),
        value if value.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn parse_window_from_lookup(
    headers: &BTreeMap<String, &str>,
    prefix: &str,
) -> Option<RateLimitWindow> {
    let used_percent = headers
        .get(&format!("{prefix}-used-percent"))
        .and_then(|value| parse_finite_percent(value))?;
    let window_minutes = headers
        .get(&format!("{prefix}-window-minutes"))
        .and_then(|value| parse_positive_u64(value));
    let reset_at = headers
        .get(&format!("{prefix}-reset-at"))
        .and_then(|value| parse_positive_i64(value));

    Some(RateLimitWindow {
        used_percent,
        window_minutes,
        reset_at,
    })
}

fn parse_details_from_object(
    value: &Value,
    limit_id: &str,
    limit_name: Option<String>,
) -> Option<RateLimitDetails> {
    let primary = value.get("primary").and_then(parse_window_from_object);
    let secondary = value.get("secondary").and_then(parse_window_from_object);
    let allowed = value.get("allowed").and_then(Value::as_bool);
    let limit_reached = value.get("limit_reached").and_then(Value::as_bool);
    if primary.is_none() && secondary.is_none() && allowed.is_none() && limit_reached.is_none() {
        return None;
    }
    Some(RateLimitDetails {
        limit_id: limit_id.to_string(),
        limit_name,
        allowed,
        limit_reached,
        primary,
        secondary,
    })
}

fn parse_window_from_object(value: &Value) -> Option<RateLimitWindow> {
    let used_percent = value
        .get("used_percent")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())?;
    let window_minutes = value.get("window_minutes").and_then(value_as_positive_u64);
    let reset_at = value.get("reset_at").and_then(value_as_positive_i64);

    Some(RateLimitWindow {
        used_percent,
        window_minutes,
        reset_at,
    })
}

fn quota_snapshot_from_windows(
    source: &str,
    limit_name: Option<&str>,
    metered_feature: Option<&str>,
    primary: Option<RateLimitWindow>,
    secondary: Option<RateLimitWindow>,
    allowed: Option<bool>,
    limit_reached: Option<bool>,
) -> Option<Value> {
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    let blocked = limit_reached.unwrap_or(false)
        || allowed.is_some_and(|allowed| !allowed)
        || primary.is_some_and(|window| window.used_percent >= 100.0)
        || secondary.is_some_and(|window| window.used_percent >= 100.0);
    Some(json!({
        "source": source,
        "limit_name": limit_name,
        "metered_feature": metered_feature,
        "allowed": allowed,
        "limit_reached": limit_reached,
        "blocked": blocked,
        "primary": quota_window(primary),
        "secondary": quota_window(secondary),
    }))
}

fn quota_window(window: Option<RateLimitWindow>) -> Value {
    let Some(window) = window else {
        return Value::Null;
    };
    let limit_reached = window.used_percent >= 100.0;
    json!({
        "used_percent": window.used_percent,
        "remaining_percent": remaining_percent(window.used_percent),
        "reset_at": window.reset_at,
        "window_minutes": window.window_minutes,
        "limit_reached": limit_reached,
    })
}

fn monthly_limit_from_snapshots(snapshots: &[Value]) -> Option<Value> {
    for snapshot in snapshots {
        if snapshot.get("source").and_then(Value::as_str) != Some("core") {
            continue;
        }
        for key in ["primary", "secondary"] {
            let Some(bucket) = snapshot.get(key).filter(|value| !value.is_null()) else {
                continue;
            };
            if bucket
                .get("window_minutes")
                .and_then(Value::as_u64)
                .is_some_and(|minutes| minutes.abs_diff(43_200) <= 2_160)
            {
                return Some(json!({
                    "key": "core-monthly",
                    "source": "rate_limit",
                    "used_percent": bucket.get("used_percent").cloned().unwrap_or(Value::Null),
                    "remaining_percent": bucket.get("remaining_percent").cloned().unwrap_or(Value::Null),
                    "reset_at": bucket.get("reset_at").cloned().unwrap_or(Value::Null),
                    "window_minutes": bucket.get("window_minutes").cloned().unwrap_or(Value::Null),
                    "limit_reached": bucket.get("limit_reached").cloned().unwrap_or(Value::Bool(false)),
                    "used_credits": Value::Null,
                    "limit_credits": Value::Null,
                }));
            }
        }
    }
    None
}

fn remaining_percent(used_percent: f64) -> i64 {
    let used = used_percent.clamp(0.0, 100.0);
    (100.0 - used).round() as i64
}

fn parse_finite_percent(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_positive_u64(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok().filter(|value| *value > 0)
}

fn parse_positive_i64(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

fn value_as_positive_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .filter(|value| *value > 0)
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .filter(|value| *value > 0)
}

fn value_as_positive_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .filter(|value| *value > 0)
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .filter(|value| *value > 0)
}

fn rate_limit_name(value: &Value) -> Option<&str> {
    value
        .get("metered_limit_name")
        .or_else(|| value.get("limit_name"))
        .and_then(Value::as_str)
}

fn normalize_limit_id(value: impl AsRef<str>) -> String {
    value.as_ref().trim().to_ascii_lowercase().replace('-', "_")
}

fn is_review_limit_name(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "review" | "code_review" | "codex_review" | "codex_code_review"
    )
}
