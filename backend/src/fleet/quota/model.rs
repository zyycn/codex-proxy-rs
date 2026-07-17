//! fleet 唯一拥有的 typed quota 快照。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const FIVE_HOUR_WINDOW_MINUTES: u64 = 300;
pub const WEEK_WINDOW_MINUTES: u64 = 10_080;
pub const MONTH_WINDOW_MINUTES: u64 = 43_200;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindowKind {
    ShortTerm,
    Weekly,
    Monthly,
    #[default]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWindowRole {
    Primary,
    Secondary,
    Monthly,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaWindow {
    #[serde(skip)]
    pub kind: QuotaWindowKind,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<i64>,
    pub reset_at: Option<i64>,
    pub window_minutes: Option<u64>,
    #[serde(default)]
    pub limit_reached: bool,
    pub used: Option<Value>,
    pub limit: Option<Value>,
    pub used_credits: Option<Value>,
    pub limit_credits: Option<Value>,
}

#[derive(Deserialize)]
struct QuotaWindowWire {
    #[serde(default, deserialize_with = "deserialize_optional_finite_number")]
    used_percent: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_finite_number")]
    remaining_percent: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_i64")]
    reset_at: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_u64")]
    window_minutes: Option<u64>,
    #[serde(default)]
    limit_reached: bool,
    #[serde(default)]
    used: Option<Value>,
    #[serde(default)]
    limit: Option<Value>,
    #[serde(default)]
    used_credits: Option<Value>,
    #[serde(default)]
    limit_credits: Option<Value>,
}

impl<'de> Deserialize<'de> for QuotaWindow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = QuotaWindowWire::deserialize(deserializer)?;
        Ok(Self {
            kind: quota_window_kind(wire.window_minutes),
            used_percent: wire.used_percent.map(clamp_percent),
            remaining_percent: wire
                .remaining_percent
                .map(clamp_percent)
                .map(|value| value.round() as i64),
            reset_at: wire.reset_at,
            window_minutes: wire.window_minutes,
            limit_reached: wire.limit_reached,
            used: wire.used,
            limit: wire.limit,
            used_credits: wire.used_credits,
            limit_credits: wire.limit_credits,
        })
    }
}

impl QuotaWindow {
    pub fn from_usage(
        used_percent: f64,
        reset_at: Option<i64>,
        window_minutes: Option<u64>,
        used: Option<Value>,
        limit: Option<Value>,
    ) -> Self {
        let used_percent = clamp_percent(used_percent);
        Self {
            kind: quota_window_kind(window_minutes),
            used_percent: Some(used_percent),
            remaining_percent: Some(remaining_percent(used_percent)),
            reset_at,
            window_minutes,
            limit_reached: used_percent >= 100.0,
            used,
            limit,
            used_credits: None,
            limit_credits: None,
        }
    }

    pub fn from_observation(
        used_percent: f64,
        reset_at: Option<i64>,
        window_minutes: Option<u64>,
    ) -> Self {
        Self::from_usage(used_percent, reset_at, window_minutes, None, None)
    }

    pub fn window_seconds(&self) -> Option<u64> {
        self.window_minutes?.checked_mul(60)
    }

    pub fn reset_datetime(&self) -> Option<DateTime<Utc>> {
        DateTime::<Utc>::from_timestamp(self.reset_at?, 0)
    }

    pub fn is_limit_reached(&self) -> bool {
        self.limit_reached || self.used_percent.is_some_and(|used| used >= 100.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaLimitSnapshot {
    pub source: String,
    #[serde(default)]
    pub limit_name: Option<String>,
    #[serde(default)]
    pub metered_feature: Option<String>,
    #[serde(default)]
    pub allowed: Option<bool>,
    #[serde(default)]
    pub limit_reached: Option<bool>,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub primary: Option<QuotaWindow>,
    #[serde(default)]
    pub secondary: Option<QuotaWindow>,
}

impl QuotaLimitSnapshot {
    pub fn is_blocked(&self) -> bool {
        self.blocked
            || self.limit_reached.unwrap_or(false)
            || self.allowed.is_some_and(|allowed| !allowed)
            || self.windows().any(QuotaWindow::is_limit_reached)
    }

    pub fn windows(&self) -> impl Iterator<Item = &QuotaWindow> {
        [self.primary.as_ref(), self.secondary.as_ref()]
            .into_iter()
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonthlyQuotaLimit {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_finite_number")]
    pub used_percent: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_finite_number")]
    pub remaining_percent: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_i64")]
    pub reset_at: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_u64")]
    pub window_minutes: Option<u64>,
    #[serde(default)]
    pub limit_reached: bool,
    #[serde(default)]
    pub used: Option<Value>,
    #[serde(default)]
    pub limit: Option<Value>,
    #[serde(default)]
    pub used_credits: Option<Value>,
    #[serde(default)]
    pub limit_credits: Option<Value>,
}

impl MonthlyQuotaLimit {
    pub fn as_window(&self) -> QuotaWindow {
        QuotaWindow {
            kind: QuotaWindowKind::Monthly,
            used_percent: self.used_percent.map(clamp_percent),
            remaining_percent: self
                .remaining_percent
                .map(clamp_percent)
                .map(|value| value.round() as i64),
            reset_at: self.reset_at,
            window_minutes: self.window_minutes.or(Some(MONTH_WINDOW_MINUTES)),
            limit_reached: self.limit_reached,
            used: self.used.clone(),
            limit: self.limit.clone(),
            used_credits: self.used_credits.clone(),
            limit_credits: self.limit_credits.clone(),
        }
    }

    pub fn is_limit_reached(&self) -> bool {
        self.limit_reached || self.used_percent.is_some_and(|used| used >= 100.0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaCredits {
    #[serde(default)]
    pub has_credits: bool,
    #[serde(default)]
    pub unlimited: bool,
    #[serde(default)]
    pub overage_limit_reached: bool,
    #[serde(default)]
    pub balance: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaSpendLimit {
    #[serde(default, deserialize_with = "deserialize_optional_finite_number")]
    pub used_percent: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_finite_number")]
    pub remaining_percent: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_i64")]
    pub reset_at: Option<i64>,
    #[serde(default)]
    pub used: Option<Value>,
    #[serde(default)]
    pub limit: Option<Value>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaSpendControl {
    #[serde(default)]
    pub reached: bool,
    #[serde(default)]
    pub individual_limit: Option<QuotaSpendLimit>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    #[serde(default)]
    pub plan_type: Option<String>,
    #[serde(default)]
    pub active_limit: Option<String>,
    #[serde(default, rename = "snapshots")]
    pub limits: Vec<QuotaLimitSnapshot>,
    #[serde(default)]
    pub monthly_limit: Option<MonthlyQuotaLimit>,
    #[serde(default)]
    pub credits: Option<QuotaCredits>,
    #[serde(default)]
    pub spend_control: Option<QuotaSpendControl>,
    #[serde(default)]
    pub promo_message: Option<String>,
    #[serde(default)]
    pub rate_limit_reached_type: Option<String>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl QuotaSnapshot {
    pub fn from_json(value: &str) -> serde_json::Result<Self> {
        serde_json::from_str(value)
    }

    pub fn from_value(value: Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    pub fn is_limit_reached(&self) -> bool {
        self.limits.iter().any(QuotaLimitSnapshot::is_blocked)
            || self
                .monthly_limit
                .as_ref()
                .is_some_and(MonthlyQuotaLimit::is_limit_reached)
            || self
                .spend_control
                .as_ref()
                .is_some_and(quota_spend_control_limit_reached)
            || self
                .credits
                .as_ref()
                .is_some_and(|credits| credits.overage_limit_reached)
    }

    pub fn reset_at(&self) -> Option<DateTime<Utc>> {
        let reset_at = if self.monthly_or_spend_limit_reached() {
            self.monthly_reset_at()
                .or_else(|| self.blocking_window().and_then(|window| window.reset_at))
                .or_else(|| {
                    self.core_window_with_reset()
                        .and_then(|window| window.reset_at)
                })
        } else if let Some(reset_at) = self.blocking_window().and_then(|window| window.reset_at) {
            Some(reset_at)
        } else {
            self.core_window_with_reset()
                .and_then(|window| window.reset_at)
                .or_else(|| self.monthly_reset_at())
        }?;
        DateTime::<Utc>::from_timestamp(reset_at, 0)
    }

    pub fn limit_window_seconds(&self) -> Option<u64> {
        if self.monthly_or_spend_limit_reached() {
            return self
                .monthly_window_seconds()
                .or(MONTH_WINDOW_MINUTES.checked_mul(60));
        }
        self.blocking_window()
            .and_then(QuotaWindow::window_seconds)
            .or_else(|| self.core_windows().find_map(QuotaWindow::window_seconds))
            .or_else(|| self.monthly_window_seconds())
    }

    pub fn windows(&self) -> impl Iterator<Item = QuotaWindowEntry<'_>> {
        let monthly = self.monthly_limit.as_ref().map(|limit| QuotaWindowEntry {
            source: limit.source.as_deref().unwrap_or("monthly"),
            limit_name: limit.key.as_deref(),
            metered_feature: None,
            role: QuotaWindowRole::Monthly,
            window: QuotaWindowRef::Monthly(limit),
        });
        monthly
            .into_iter()
            .chain(self.limits.iter().flat_map(|limit| {
                let primary = limit.primary.as_ref().map(|window| QuotaWindowEntry {
                    source: &limit.source,
                    limit_name: limit.limit_name.as_deref(),
                    metered_feature: limit.metered_feature.as_deref(),
                    role: QuotaWindowRole::Primary,
                    window: QuotaWindowRef::Regular(window),
                });
                let secondary = limit.secondary.as_ref().map(|window| QuotaWindowEntry {
                    source: &limit.source,
                    limit_name: limit.limit_name.as_deref(),
                    metered_feature: limit.metered_feature.as_deref(),
                    role: QuotaWindowRole::Secondary,
                    window: QuotaWindowRef::Regular(window),
                });
                primary.into_iter().chain(secondary)
            }))
    }

    fn monthly_or_spend_limit_reached(&self) -> bool {
        self.monthly_limit
            .as_ref()
            .is_some_and(MonthlyQuotaLimit::is_limit_reached)
            || self
                .spend_control
                .as_ref()
                .is_some_and(quota_spend_control_limit_reached)
    }

    fn monthly_reset_at(&self) -> Option<i64> {
        self.monthly_limit
            .as_ref()?
            .reset_at
            .filter(|value| *value > 0)
    }

    fn monthly_window_seconds(&self) -> Option<u64> {
        self.monthly_limit
            .as_ref()?
            .window_minutes
            .filter(|value| *value > 0)?
            .checked_mul(60)
    }

    fn blocking_window(&self) -> Option<&QuotaWindow> {
        self.limits
            .iter()
            .filter(|limit| limit.is_blocked())
            .flat_map(QuotaLimitSnapshot::windows)
            .find(|window| window.is_limit_reached())
            .or_else(|| {
                self.limits
                    .iter()
                    .filter(|limit| limit.is_blocked())
                    .flat_map(QuotaLimitSnapshot::windows)
                    .find(|window| window.reset_at.is_some())
            })
    }

    fn core_window_with_reset(&self) -> Option<&QuotaWindow> {
        self.core_windows().find(|window| window.reset_at.is_some())
    }

    fn core_windows(&self) -> impl Iterator<Item = &QuotaWindow> {
        self.limits
            .iter()
            .filter(|limit| limit.source == "core")
            .flat_map(QuotaLimitSnapshot::windows)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum QuotaWindowRef<'a> {
    Regular(&'a QuotaWindow),
    Monthly(&'a MonthlyQuotaLimit),
}

impl QuotaWindowRef<'_> {
    pub fn to_owned(self) -> QuotaWindow {
        match self {
            Self::Regular(window) => window.clone(),
            Self::Monthly(limit) => limit.as_window(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QuotaWindowEntry<'a> {
    pub source: &'a str,
    pub limit_name: Option<&'a str>,
    pub metered_feature: Option<&'a str>,
    pub role: QuotaWindowRole,
    pub window: QuotaWindowRef<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassifiedQuotaWindow {
    pub key: String,
    pub source: String,
    pub limit_name: Option<String>,
    pub metered_feature: Option<String>,
    pub role: QuotaWindowRole,
    pub window: QuotaWindow,
    pub order: u8,
}

impl QuotaSnapshot {
    pub fn classified_windows(&self) -> Vec<ClassifiedQuotaWindow> {
        let has_monthly_limit = self.monthly_limit.is_some();
        let mut windows = Vec::new();
        for entry in self.windows() {
            let window = entry.window.to_owned();
            if has_monthly_limit
                && entry.source == "core"
                && entry.role != QuotaWindowRole::Monthly
                && window.kind == QuotaWindowKind::Monthly
            {
                continue;
            }
            let has_semantic_label = entry
                .limit_name
                .or(entry.metered_feature)
                .is_some_and(|label| !label.trim().is_empty());
            if window.reset_at.is_none()
                && window.window_minutes.is_none()
                && (window.used_percent.is_none() || !has_semantic_label)
            {
                continue;
            }
            let source_key =
                quota_source_key(entry.source, entry.limit_name, entry.metered_feature);
            let bucket = quota_window_key_part(window.kind)
                .unwrap_or_else(|| quota_window_role_name(entry.role));
            let mut key = format!("{source_key}-{bucket}");
            if windows
                .iter()
                .any(|existing: &ClassifiedQuotaWindow| existing.key == key)
            {
                key = format!("{key}-{}", quota_window_role_name(entry.role));
            }
            windows.push(ClassifiedQuotaWindow {
                key,
                source: entry.source.to_string(),
                limit_name: entry.limit_name.map(ToString::to_string),
                metered_feature: entry.metered_feature.map(ToString::to_string),
                role: entry.role,
                order: quota_window_order(window.kind),
                window,
            });
        }
        windows.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| {
                    left.window
                        .window_minutes
                        .unwrap_or_default()
                        .cmp(&right.window.window_minutes.unwrap_or_default())
                })
                .then_with(|| left.key.cmp(&right.key))
        });
        windows
    }

    pub fn representative_used_percent(&self) -> Option<f64> {
        self.classified_windows()
            .into_iter()
            .filter_map(|window| {
                window
                    .window
                    .used_percent
                    .map(|used| (quota_usage_priority(window.window.kind), used))
            })
            .fold(None::<(u8, f64)>, |selected, candidate| match selected {
                Some(current)
                    if current.0 < candidate.0
                        || (current.0 == candidate.0 && current.1 >= candidate.1) =>
                {
                    Some(current)
                }
                _ => Some(candidate),
            })
            .map(|(_, used)| used)
    }
}

pub fn quota_window_kind(window_minutes: Option<u64>) -> QuotaWindowKind {
    match window_minutes {
        Some(minutes) if window_minutes_match(minutes, FIVE_HOUR_WINDOW_MINUTES) => {
            QuotaWindowKind::ShortTerm
        }
        Some(minutes) if window_minutes_match(minutes, WEEK_WINDOW_MINUTES) => {
            QuotaWindowKind::Weekly
        }
        Some(minutes) if window_minutes_match(minutes, MONTH_WINDOW_MINUTES) => {
            QuotaWindowKind::Monthly
        }
        _ => QuotaWindowKind::Other,
    }
}

pub fn window_minutes_match(actual: u64, expected: u64) -> bool {
    actual > 0 && actual.abs_diff(expected) <= expected / 20
}

fn quota_window_order(kind: QuotaWindowKind) -> u8 {
    match kind {
        QuotaWindowKind::Monthly => 0,
        QuotaWindowKind::ShortTerm => 1,
        QuotaWindowKind::Weekly => 2,
        QuotaWindowKind::Other => 3,
    }
}

fn quota_usage_priority(kind: QuotaWindowKind) -> u8 {
    match kind {
        QuotaWindowKind::ShortTerm => 0,
        QuotaWindowKind::Weekly => 1,
        QuotaWindowKind::Monthly => 2,
        QuotaWindowKind::Other => 3,
    }
}

fn quota_window_key_part(kind: QuotaWindowKind) -> Option<&'static str> {
    match kind {
        QuotaWindowKind::ShortTerm => Some("five-hour"),
        QuotaWindowKind::Weekly => Some("weekly"),
        QuotaWindowKind::Monthly => Some("monthly"),
        QuotaWindowKind::Other => None,
    }
}

fn quota_window_role_name(role: QuotaWindowRole) -> &'static str {
    match role {
        QuotaWindowRole::Primary => "primary",
        QuotaWindowRole::Secondary => "secondary",
        QuotaWindowRole::Monthly => "monthly",
    }
}

fn quota_source_key(
    source: &str,
    limit_name: Option<&str>,
    metered_feature: Option<&str>,
) -> String {
    let label = limit_name.or(metered_feature).unwrap_or(source);
    format!("{}-{}", quota_key_segment(source), quota_key_segment(label))
}

fn quota_key_segment(value: &str) -> String {
    let mut segment = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            segment.push(ch.to_ascii_lowercase());
        } else if !segment.ends_with('-') {
            segment.push('-');
        }
    }
    let segment = segment.trim_matches('-');
    if segment.is_empty() {
        "quota".to_string()
    } else {
        segment.to_string()
    }
}

pub fn remaining_percent(used_percent: f64) -> i64 {
    (100.0 - clamp_percent(used_percent)).round() as i64
}

fn quota_spend_control_limit_reached(spend_control: &QuotaSpendControl) -> bool {
    spend_control.reached
        || spend_control
            .individual_limit
            .as_ref()
            .and_then(|limit| limit.used_percent)
            .is_some_and(|used| used >= 100.0)
}

fn clamp_percent(value: f64) -> f64 {
    value.clamp(0.0, 100.0)
}

fn deserialize_optional_finite_number<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
            .filter(|value| value.is_finite())
    }))
}

fn deserialize_optional_positive_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
                .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
        })
        .filter(|value| *value > 0))
}

fn deserialize_optional_positive_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
        })
        .filter(|value| *value > 0))
}
