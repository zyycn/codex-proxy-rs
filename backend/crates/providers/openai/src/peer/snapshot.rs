//! 互联响应的有界校验、能力授权与时间有效性解释

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use gateway_core::account::{OpaqueProviderData, UpstreamInfoFailure};
use gateway_protocol::peer::{
    MAX_ACCOUNTS, MAX_MODELS, MAX_QUOTA_WINDOWS, OBJECT, PeerLink, PeerPoolDisclosure,
    SCHEMA_VERSION,
};
use serde::Deserialize as _;

pub(super) const MAX_AGE: Duration = Duration::from_secs(30 * 60);
pub(super) const FRESH_FOR: Duration = Duration::from_secs(5 * 60);

pub(super) fn timestamp(value: &str) -> Option<SystemTime> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(SystemTime::from)
}

pub(super) fn datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

pub(super) fn fresh(observed: &str, now: SystemTime) -> bool {
    timestamp(observed).is_some_and(|observed| {
        observed <= now + Duration::from_secs(60)
            && now.duration_since(observed).unwrap_or_default() <= FRESH_FOR
    })
}

pub(super) fn component_fresh(
    observed: Option<&str>,
    expires: Option<&str>,
    now: SystemTime,
) -> bool {
    let Some(observed) = observed.and_then(timestamp) else {
        return false;
    };
    let expiry = match expires {
        Some(value) => match timestamp(value) {
            Some(expiry) => expiry,
            None => return false,
        },
        None => observed + FRESH_FOR,
    };
    observed <= now + Duration::from_secs(60) && expiry > now && expiry <= observed + MAX_AGE
}

pub(super) fn decode(data: &OpaqueProviderData) -> Option<PeerLink> {
    PeerLink::deserialize(
        serde::de::value::MapDeserializer::<_, serde_json::Error>::new(
            data.expose_to_provider()
                .iter()
                .map(|(key, value)| (key.as_str(), value)),
        ),
    )
    .ok()
}

pub(super) fn parse(
    bytes: &[u8],
    now: SystemTime,
) -> Result<OpaqueProviderData, UpstreamInfoFailure> {
    let mut peer: PeerLink =
        serde_json::from_slice(bytes).map_err(|_| UpstreamInfoFailure::InvalidResponse)?;
    let valid_text = |value: &str, max: usize| {
        !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    };
    let observed = timestamp(&peer.observed_at).ok_or(UpstreamInfoFailure::InvalidResponse)?;
    let expires = timestamp(&peer.expires_at).ok_or(UpstreamInfoFailure::InvalidResponse)?;
    if peer.object != OBJECT
        || peer.schema_version != SCHEMA_VERSION
        || !valid_text(&peer.node.id, 256)
        || !valid_text(&peer.node.name, 256)
        || !valid_text(&peer.node.software, 128)
        || observed > now + Duration::from_secs(60)
        || now.duration_since(observed).unwrap_or_default() > MAX_AGE
        || expires <= observed
        || expires > observed + MAX_AGE
        || peer.capabilities.len() > 32
        || peer
            .quota
            .accounts
            .iter()
            .map(|account| account.windows.len())
            .sum::<usize>()
            > MAX_QUOTA_WINDOWS
        || peer.quota.accounts.len() > MAX_ACCOUNTS
        || peer.models.len() > MAX_MODELS
    {
        return Err(UpstreamInfoFailure::InvalidResponse);
    }
    // 未授权能力不能借同名数据块越过逐层公开边界
    if !peer.capabilities.iter().any(|value| value == "key_usage") {
        peer.usage.data = None;
    }
    if !peer
        .capabilities
        .iter()
        .any(|value| value == "pool_summary")
    {
        peer.pool = Default::default();
    }
    if !peer
        .capabilities
        .iter()
        .any(|value| value == "quota_windows")
    {
        peer.quota = Default::default();
    }
    if !peer
        .capabilities
        .iter()
        .any(|value| value == "model_availability")
    {
        peer.models.clear();
    }
    if peer.pool.disclosure == PeerPoolDisclosure::Summary {
        let count = peer
            .pool
            .account_count
            .ok_or(UpstreamInfoFailure::InvalidResponse)?;
        let states = peer
            .pool
            .states
            .as_ref()
            .ok_or(UpstreamInfoFailure::InvalidResponse)?;
        let state_total = [
            states.ready,
            states.cooldown,
            states.exhausted,
            states.disabled,
            states.error,
            states.unknown,
        ]
        .into_iter()
        .map(u64::from)
        .sum::<u64>();
        if u64::from(count) > MAX_ACCOUNTS as u64 || state_total != u64::from(count) {
            return Err(UpstreamInfoFailure::InvalidResponse);
        }
        if let Some(plans) = &peer.pool.plans
            && (plans.len() > MAX_ACCOUNTS
                || plans
                    .iter()
                    .any(|plan| !valid_text(&plan.name, 256) || plan.count > count)
                || plans.iter().map(|plan| u64::from(plan.count)).sum::<u64>() > u64::from(count))
        {
            return Err(UpstreamInfoFailure::InvalidResponse);
        }
    }
    if let Some(usage) = &peer.usage.data {
        if !valid_text(&usage.unit, 16) {
            return Err(UpstreamInfoFailure::InvalidResponse);
        }
        for window in [&usage.daily, &usage.weekly] {
            if [
                Some(&window.used),
                window.total.as_ref(),
                window.remaining.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| decimal(value).is_none())
                || window
                    .resets_at
                    .as_ref()
                    .is_some_and(|value| timestamp(value).is_none())
            {
                return Err(UpstreamInfoFailure::InvalidResponse);
            }
        }
    }
    for account in &peer.quota.accounts {
        if !valid_text(&account.id, 256)
            || account.models.len() > MAX_MODELS
            || account.windows.iter().any(|window| {
                !valid_text(&window.key, 256)
                    || !valid_text(&window.label, 256)
                    || !valid_text(&window.group, 128)
                    || window
                        .limit_id
                        .as_deref()
                        .is_some_and(|value| !valid_text(value, 256))
                    || window
                        .limit_name
                        .as_deref()
                        .is_some_and(|value| !valid_text(value, 256))
                    || window
                        .used_percent
                        .is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value))
                    || window
                        .reset_at
                        .as_ref()
                        .is_some_and(|value| timestamp(value).is_none())
            })
        {
            return Err(UpstreamInfoFailure::InvalidResponse);
        }
    }
    for model in &peer.models {
        if !valid_text(&model.model, 256)
            || model
                .observed_at
                .as_deref()
                .is_some_and(|value| timestamp(value).is_none())
            || model
                .expires_at
                .as_deref()
                .is_some_and(|value| timestamp(value).is_none())
        {
            return Err(UpstreamInfoFailure::InvalidResponse);
        }
    }
    match serde_json::to_value(peer) {
        Ok(serde_json::Value::Object(object)) => Ok(OpaqueProviderData::new(object)),
        _ => Err(UpstreamInfoFailure::InvalidResponse),
    }
}

pub(super) fn decimal(value: &str) -> Option<f64> {
    if value.is_empty()
        || value.len() > 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && byte != b'.')
        || value.bytes().filter(|byte| *byte == b'.').count() > 1
    {
        return None;
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

pub(super) fn budget_exhausted(
    window: &gateway_protocol::peer::PeerBudgetWindow,
    now: SystemTime,
) -> bool {
    let Some(total) = window.total.as_deref() else {
        return false;
    };
    if compare_decimal(total, "0") != std::cmp::Ordering::Greater
        || window
            .resets_at
            .as_deref()
            .and_then(timestamp)
            .is_some_and(|reset| reset <= now)
    {
        return false;
    }
    window
        .remaining
        .as_deref()
        .is_some_and(|remaining| compare_decimal(remaining, "0") == std::cmp::Ordering::Equal)
        || compare_decimal(&window.used, total) != std::cmp::Ordering::Less
}

// 金额触顶比较保留十进制精度，百分比展示的浮点近似不能成为耗尽依据
fn compare_decimal(left: &str, right: &str) -> std::cmp::Ordering {
    let parts = |value: &str| {
        let (integer, fractional) = value.split_once('.').unwrap_or((value, ""));
        (
            integer.trim_start_matches('0').to_owned(),
            fractional.to_owned(),
        )
    };
    let (left_integer, left_fractional) = parts(left);
    let (right_integer, right_fractional) = parts(right);
    left_integer
        .len()
        .cmp(&right_integer.len())
        .then_with(|| left_integer.cmp(&right_integer))
        .then_with(|| {
            let width = left_fractional.len().max(right_fractional.len());
            left_fractional
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(width)
                .cmp(
                    right_fractional
                        .bytes()
                        .chain(std::iter::repeat(b'0'))
                        .take(width),
                )
        })
}
