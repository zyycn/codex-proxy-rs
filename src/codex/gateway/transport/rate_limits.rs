use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimitWindow {
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub reset_at: Option<i64>,
}

impl RateLimitWindow {
    pub fn limit_window_seconds(self) -> Option<u64> {
        self.window_minutes
            .and_then(|minutes| minutes.checked_mul(60))
    }

    pub fn reset_at_datetime(self) -> Option<DateTime<Utc>> {
        let reset_at = self.reset_at?;
        DateTime::<Utc>::from_timestamp(reset_at, 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimitDetails {
    pub allowed: Option<bool>,
    pub limit_reached: Option<bool>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedRateLimits {
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub code_review: Option<RateLimitDetails>,
}

impl ParsedRateLimits {
    pub fn primary_reset_at(self) -> Option<DateTime<Utc>> {
        self.primary.and_then(RateLimitWindow::reset_at_datetime)
    }

    pub fn primary_limit_window_seconds(self) -> Option<u64> {
        self.primary.and_then(RateLimitWindow::limit_window_seconds)
    }

    pub fn primary_limit_reached(self) -> bool {
        self.primary
            .is_some_and(|window| window.used_percent >= 100.0)
    }
}

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

pub fn parse_rate_limits_event_raw(raw: &str) -> Option<ParsedRateLimits> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| parse_rate_limits_event(&value))
}

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        parse_rate_limit_headers, parse_rate_limits_event, rate_limit_quota, RateLimitWindow,
    };

    #[test]
    fn parse_rate_limit_headers_should_extract_primary_secondary_and_review_windows() {
        let headers = vec![
            (
                "x-codex-primary-used-percent".to_string(),
                "100".to_string(),
            ),
            (
                "x-codex-primary-window-minutes".to_string(),
                "5".to_string(),
            ),
            (
                "x-codex-primary-reset-at".to_string(),
                "1893456300".to_string(),
            ),
            (
                "x-codex-secondary-used-percent".to_string(),
                "42.5".to_string(),
            ),
            (
                "x-codex-secondary-window-minutes".to_string(),
                "10080".to_string(),
            ),
            (
                "x-codex-code-review-primary-used-percent".to_string(),
                "80".to_string(),
            ),
            (
                "x-codex-code-review-primary-reset-at".to_string(),
                "1893456600".to_string(),
            ),
        ];

        let parsed = parse_rate_limit_headers(&headers).unwrap();

        assert_eq!(
            parsed.primary,
            Some(RateLimitWindow {
                used_percent: 100.0,
                window_minutes: Some(5),
                reset_at: Some(1_893_456_300),
            })
        );
        assert_eq!(parsed.secondary.unwrap().window_minutes, Some(10080));
        assert_eq!(
            parsed.code_review.unwrap().primary.unwrap().reset_at,
            Some(1_893_456_600)
        );
    }

    #[test]
    fn parse_rate_limits_event_should_extract_internal_websocket_rate_limits() {
        let event = json!({
            "type": "codex.rate_limits",
            "rate_limits": {
                "primary": {
                    "used_percent": 99.5,
                    "window_minutes": 300,
                    "reset_at": 1893456300
                },
                "secondary": {
                    "used_percent": 10,
                    "window_minutes": 10080,
                    "reset_at": 1894056000
                }
            }
        });

        let parsed = parse_rate_limits_event(&event).unwrap();

        assert_eq!(parsed.primary.unwrap().used_percent, 99.5);
        assert_eq!(parsed.secondary.unwrap().reset_at, Some(1_894_056_000));
    }

    #[test]
    fn rate_limit_quota_should_preserve_existing_credits_when_passive_data_lacks_credits() {
        let headers = vec![
            ("x-codex-primary-used-percent".to_string(), "25".to_string()),
            (
                "x-codex-primary-window-minutes".to_string(),
                "5".to_string(),
            ),
            (
                "x-codex-primary-reset-at".to_string(),
                "1893456300".to_string(),
            ),
        ];
        let existing = json!({
            "credits": {
                "has_credits": true,
                "unlimited": false,
                "balance": 12
            }
        });
        let parsed = parse_rate_limit_headers(&headers).unwrap();

        let quota = rate_limit_quota(&parsed, Some("plus"), Some(&existing));

        assert_eq!(quota["plan_type"], "plus");
        assert_eq!(quota["rate_limit"]["remaining_percent"], 75);
        assert_eq!(quota["credits"]["balance"], 12);
    }
}
