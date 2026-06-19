//! Codex 事件语义、用量提取与限流解析。

use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::protocol::codex::sse::{parse_sse_events, SseError};

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
    /// 图片工具输入 token 数。
    pub image_input_tokens: u64,
    /// 图片工具输出 token 数。
    pub image_output_tokens: u64,
    /// 主模型总 token 数，不包含图片工具 token。
    pub total_tokens: u64,
}

impl TokenUsage {
    /// 构造仅包含主模型 token 的用量快照。
    pub fn new(input_tokens: u64, output_tokens: u64, cached_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_tokens,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: input_tokens + output_tokens,
        }
    }

    /// 将两份用量按字段累加。
    pub fn merged(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cached_tokens: self.cached_tokens + other.cached_tokens,
            image_input_tokens: self.image_input_tokens + other.image_input_tokens,
            image_output_tokens: self.image_output_tokens + other.image_output_tokens,
            total_tokens: self.total_tokens + other.total_tokens,
        }
    }
}

/// 从单个 JSON 响应体中提取用量。
///
/// 该函数同时兼容 Codex usage 结构和 OpenAI usage 结构。
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
        "total_tokens",
    ]
    .iter()
    .any(|field| usage.get(*field).is_some())
        || usage.get("input_tokens_details").is_some()
        || usage.get("prompt_tokens_details").is_some()
        || image_input_tokens > 0
        || image_output_tokens > 0;

    has_usage.then_some(TokenUsage {
        input_tokens,
        output_tokens,
        cached_tokens,
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
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .unwrap_or(&value);
    if let Some(seconds) = error
        .get("resets_in_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
    {
        return Some(seconds);
    }
    retry_after_seconds_from_resets_at(error)
        .or_else(|| retry_after_seconds_from_rate_limit_message(error))
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

impl RateLimitWindow {
    /// 以秒返回限流窗口大小。
    pub fn limit_window_seconds(self) -> Option<u64> {
        self.window_minutes
            .and_then(|minutes| minutes.checked_mul(60))
    }

    /// 将重置时间戳转换为 UTC 时间。
    pub fn reset_at_datetime(self) -> Option<DateTime<Utc>> {
        let reset_at = self.reset_at?;
        DateTime::<Utc>::from_timestamp(reset_at, 0)
    }
}

/// 复合限流信息，通常用于 code review 配额。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimitDetails {
    /// 当前请求是否被允许。
    pub allowed: Option<bool>,
    /// 当前窗口是否已经触顶。
    pub limit_reached: Option<bool>,
    /// 主限流窗口。
    pub primary: Option<RateLimitWindow>,
    /// 次级限流窗口。
    pub secondary: Option<RateLimitWindow>,
}

/// 从 header 或内部事件中解析出的完整限流状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedRateLimits {
    /// 主限流窗口。
    pub primary: Option<RateLimitWindow>,
    /// 次级限流窗口。
    pub secondary: Option<RateLimitWindow>,
    /// code review 限流窗口。
    pub code_review: Option<RateLimitDetails>,
}

impl ParsedRateLimits {
    /// 返回主窗口的 UTC 重置时间。
    pub fn primary_reset_at(self) -> Option<DateTime<Utc>> {
        self.primary.and_then(RateLimitWindow::reset_at_datetime)
    }

    /// 返回主窗口的秒级窗口长度。
    pub fn primary_limit_window_seconds(self) -> Option<u64> {
        self.primary.and_then(RateLimitWindow::limit_window_seconds)
    }

    /// 返回主窗口是否已经触顶。
    pub fn primary_limit_reached(self) -> bool {
        self.primary
            .is_some_and(|window| window.used_percent >= 100.0)
    }
}

/// 从响应头对中解析限流信息。
pub fn parse_rate_limit_headers(headers: &[(String, String)]) -> Option<ParsedRateLimits> {
    let mut normalized = BTreeMap::new();
    for (name, value) in headers {
        normalized.insert(name.to_ascii_lowercase(), value.trim());
    }

    let primary = parse_window_from_lookup(&normalized, "x-codex-primary");
    let secondary = parse_window_from_lookup(&normalized, "x-codex-secondary");
    let code_review = parse_details_from_lookup(&normalized, "x-codex-code-review")
        .or_else(|| parse_details_from_lookup(&normalized, "x-codex-review"))
        .or_else(|| parse_details_from_lookup(&normalized, "x-code-review"));

    if primary.is_none() && secondary.is_none() && code_review.is_none() {
        return None;
    }

    Some(ParsedRateLimits {
        primary,
        secondary,
        code_review,
    })
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

    let details = value.get("rate_limits").and_then(parse_details_from_object);
    let explicit_code_review = value
        .get("code_review_rate_limits")
        .and_then(parse_details_from_object)
        .or_else(|| {
            value
                .get("code_review_rate_limit")
                .and_then(parse_details_from_object)
        });

    let mut primary = details.and_then(|details| details.primary);
    let mut secondary = details.and_then(|details| details.secondary);
    let mut code_review = explicit_code_review;
    if details.is_some() && is_review_limit_name(rate_limit_name(value)) {
        code_review = code_review.or(details);
        primary = None;
        secondary = None;
    }

    if primary.is_none() && secondary.is_none() && code_review.is_none() {
        return None;
    }

    Some(ParsedRateLimits {
        primary,
        secondary,
        code_review,
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
    let mut quota = json!({
        "plan_type": plan_type.unwrap_or("unknown"),
        "rate_limit": quota_window(rate_limits.primary, true),
        "secondary_rate_limit": rate_limits.secondary.map(|window| quota_window(Some(window), false)).unwrap_or(Value::Null),
        "code_review_rate_limit": rate_limits.code_review.map(code_review_quota_window).unwrap_or(Value::Null),
    });

    if let Some(credits) = existing_quota.and_then(|quota| quota.get("credits")) {
        quota["credits"] = credits.clone();
    }

    quota
}

/// 将限流状态转换回 HTTP 头键值对。
pub fn rate_limits_to_header_pairs(rate_limits: &ParsedRateLimits) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    push_window_headers(&mut headers, "x-codex-primary", rate_limits.primary);
    push_window_headers(&mut headers, "x-codex-secondary", rate_limits.secondary);
    if let Some(code_review) = rate_limits.code_review {
        push_window_headers(
            &mut headers,
            "x-codex-code-review-primary",
            code_review.primary,
        );
        push_window_headers(
            &mut headers,
            "x-codex-code-review-secondary",
            code_review.secondary,
        );
    }
    headers
}

/// 为退避秒数增加抖动，避免同一时间窗口内集中重试。
pub fn cooldown_with_jitter(seconds: u64, factor_bps: u16) -> Duration {
    let factor_bps = u64::from(factor_bps);
    let variance = seconds.saturating_mul(factor_bps) / 10_000;
    if variance == 0 {
        return Duration::seconds(seconds.min(i64::MAX as u64) as i64);
    }
    let lower = seconds.saturating_sub(variance);
    let span = variance.saturating_mul(2).saturating_add(1);
    let offset = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::from(duration.subsec_nanos()) % span)
        .unwrap_or(variance);
    Duration::seconds(lower.saturating_add(offset).min(i64::MAX as u64) as i64)
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
    if code != "rate_limit_exceeded" {
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
    if !seconds.is_finite() || seconds <= 0.0 || seconds > u64::MAX as f64 {
        return None;
    }
    Some(seconds.ceil() as u64)
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
) -> Option<RateLimitDetails> {
    let primary = parse_window_from_lookup(headers, &format!("{prefix}-primary"));
    let secondary = parse_window_from_lookup(headers, &format!("{prefix}-secondary"));
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(RateLimitDetails {
        allowed: None,
        limit_reached: None,
        primary,
        secondary,
    })
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

fn parse_details_from_object(value: &Value) -> Option<RateLimitDetails> {
    let primary = value.get("primary").and_then(parse_window_from_object);
    let secondary = value.get("secondary").and_then(parse_window_from_object);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(RateLimitDetails {
        allowed: value.get("allowed").and_then(Value::as_bool),
        limit_reached: value.get("limit_reached").and_then(Value::as_bool),
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

fn quota_window(window: Option<RateLimitWindow>, include_allowed: bool) -> Value {
    let Some(window) = window else {
        return Value::Null;
    };
    let limit_reached = window.used_percent >= 100.0;
    let mut value = json!({
        "used_percent": window.used_percent,
        "remaining_percent": remaining_percent(window.used_percent),
        "reset_at": window.reset_at,
        "limit_window_seconds": window.limit_window_seconds(),
        "limit_reached": limit_reached,
    });
    if include_allowed {
        value["allowed"] = Value::Bool(!limit_reached);
    }
    value
}

fn code_review_quota_window(details: RateLimitDetails) -> Value {
    let used_percent = details.primary.map(|window| window.used_percent);
    let limit_reached = details
        .limit_reached
        .unwrap_or_else(|| used_percent.is_some_and(|used| used >= 100.0));
    json!({
        "allowed": details.allowed.unwrap_or(true),
        "limit_reached": limit_reached,
        "used_percent": used_percent,
        "remaining_percent": used_percent.map(remaining_percent),
        "reset_at": details.primary.and_then(|window| window.reset_at),
        "limit_window_seconds": details.primary.and_then(RateLimitWindow::limit_window_seconds),
    })
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
