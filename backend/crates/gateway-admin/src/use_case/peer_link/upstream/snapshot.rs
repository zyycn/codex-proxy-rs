//! 互联响应的有界校验、能力授权与时间有效性解释

use std::{
    collections::HashSet,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};
use gateway_core::account::{OpaqueProviderData, UpstreamConnection, UpstreamInfoFailure};
use gateway_protocol::peer::{
    MAX_ACCOUNTS, MAX_MODELS, MAX_QUOTA_WINDOWS, OBJECT, PeerKeyUsage, PeerLink, PeerPool,
    PeerQuota, SCHEMA_VERSION,
};
use serde::Deserialize as _;

pub(super) const MAX_AGE: Duration = Duration::from_secs(30 * 60);
pub(super) const FRESH_FOR: Duration = Duration::from_secs(5 * 60);

// 不支持协议是连接的识别结果，复用观测存储与版本保护，不伪造上游额度快照
pub(super) fn unsupported(connection: &UpstreamConnection) -> OpaqueProviderData {
    OpaqueProviderData::new(serde_json::Map::from_iter([
        ("object".into(), "cpr.peer.unsupported".into()),
        ("schema_version".into(), 1.into()),
        (
            "connection_revision".into(),
            connection.revision.clone().into(),
        ),
    ]))
}

pub(super) fn is_unsupported(data: &OpaqueProviderData, connection: &UpstreamConnection) -> bool {
    let fields = data.expose_to_provider();
    (fields.get("object").and_then(serde_json::Value::as_str) == Some("cpr.peer.unsupported")
        && fields
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
        && fields
            .get("connection_revision")
            .and_then(serde_json::Value::as_str)
            == Some(connection.revision.as_str()))
        || unavailable_reason(data, connection) == Some(UpstreamInfoFailure::Unsupported)
}

// 失权后只保留同一连接已确认互联的事实，不保留已撤销的池、预算与模型数据
pub(super) fn unavailable(
    connection: &UpstreamConnection,
    reason: UpstreamInfoFailure,
) -> OpaqueProviderData {
    OpaqueProviderData::new(serde_json::Map::from_iter([
        ("object".into(), "cpr.peer.unavailable".into()),
        ("schema_version".into(), 1.into()),
        (
            "connection_revision".into(),
            connection.revision.clone().into(),
        ),
        ("reason".into(), reason.as_str().into()),
    ]))
}

pub(super) fn unavailable_reason(
    data: &OpaqueProviderData,
    connection: &UpstreamConnection,
) -> Option<UpstreamInfoFailure> {
    let fields = data.expose_to_provider();
    if fields.get("object")?.as_str()? != "cpr.peer.unavailable"
        || fields.get("schema_version")?.as_u64()? != 1
        || fields.get("connection_revision")?.as_str()? != connection.revision
    {
        return None;
    }
    match fields.get("reason")?.as_str()? {
        "unauthorized" => Some(UpstreamInfoFailure::Unauthorized),
        "unsupported" => Some(UpstreamInfoFailure::Unsupported),
        _ => None,
    }
}

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
    let peer = PeerLink::deserialize(
        serde::de::value::MapDeserializer::<_, serde_json::Error>::new(
            data.expose_to_provider()
                .iter()
                .map(|(key, value)| (key.as_str(), value)),
        ),
    )
    .ok()?;
    (peer.object == OBJECT && peer.schema_version == SCHEMA_VERSION).then_some(peer)
}

pub(super) fn parse(
    bytes: &[u8],
    now: SystemTime,
) -> Result<OpaqueProviderData, UpstreamInfoFailure> {
    let mut peer: PeerLink =
        serde_json::from_slice(bytes).map_err(|_| UpstreamInfoFailure::InvalidResponse)?;
    let observed = timestamp(&peer.observed_at).ok_or(UpstreamInfoFailure::InvalidResponse)?;
    let expires = timestamp(&peer.expires_at).ok_or(UpstreamInfoFailure::InvalidResponse)?;
    let accounts = peer
        .quota
        .as_ref()
        .map_or(&[][..], |quota| quota.accounts.as_slice());
    let budget_windows = peer
        .usage
        .as_ref()
        .and_then(|usage| usage.data.as_ref())
        .map_or(0, |usage| usage.windows.len());
    if peer.object != OBJECT
        || peer.schema_version != SCHEMA_VERSION
        || observed > now + Duration::from_secs(60)
        || now.duration_since(observed).unwrap_or_default() > MAX_AGE
        || expires <= observed
        || expires > observed + MAX_AGE
        || peer.capabilities.len() > 32
        || accounts
            .iter()
            .map(|account| account.windows.len())
            .sum::<usize>()
            > MAX_QUOTA_WINDOWS
        || budget_windows > MAX_QUOTA_WINDOWS
        || accounts.len() > MAX_ACCOUNTS
        || peer.models.len() > MAX_MODELS
    {
        return Err(UpstreamInfoFailure::InvalidResponse);
    }
    // 未授权能力不能借同名数据块越过逐层公开边界
    let disclosed = |capability| peer.capabilities.iter().any(|value| value == capability);
    if !disclosed("key_usage") {
        peer.usage = None;
    }
    if !disclosed("pool_summary") {
        peer.pool = None;
    }
    if !disclosed("quota_windows") {
        peer.quota = None;
    }
    if !disclosed("model_availability") {
        peer.models.clear();
    }
    if let Some(pool) = &peer.pool {
        validate_pool(pool)?;
    }
    if let Some(usage) = peer.usage.as_ref().and_then(|usage| usage.data.as_ref()) {
        validate_usage(usage)?;
    }
    if let Some(quota) = &peer.quota {
        validate_quota(quota)?;
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

fn valid_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn validate_pool(pool: &PeerPool) -> Result<(), UpstreamInfoFailure> {
    let count = pool.account_count;
    let states = &pool.states;
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
    if let Some(plans) = &pool.plans
        && (plans.len() > MAX_ACCOUNTS
            || plans
                .iter()
                .any(|plan| !valid_text(&plan.name, 256) || plan.count > count)
            || plans.iter().map(|plan| u64::from(plan.count)).sum::<u64>() > u64::from(count))
    {
        return Err(UpstreamInfoFailure::InvalidResponse);
    }
    Ok(())
}

fn validate_usage(usage: &PeerKeyUsage) -> Result<(), UpstreamInfoFailure> {
    if usage
        .unit
        .as_deref()
        .is_some_and(|unit| !valid_text(unit, 16))
        || !usage.windows.is_empty() && usage.unit.is_none()
    {
        return Err(UpstreamInfoFailure::InvalidResponse);
    }
    let mut keys = HashSet::new();
    for window in &usage.windows {
        if !valid_text(&window.key, 256)
            || !keys.insert(&window.key)
            || window
                .label
                .as_deref()
                .is_some_and(|label| !valid_text(label, 256))
            || window.window_seconds == Some(0)
            || [
                Some(&window.used),
                window.total.as_ref(),
                window.remaining.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| decimal(value).is_none())
            || window
                .reset_at
                .as_ref()
                .is_some_and(|value| timestamp(value).is_none())
        {
            return Err(UpstreamInfoFailure::InvalidResponse);
        }
    }
    Ok(())
}

fn validate_quota(quota: &PeerQuota) -> Result<(), UpstreamInfoFailure> {
    for account in &quota.accounts {
        if !valid_text(&account.id, 256)
            || account.models.len() > MAX_MODELS
            || account.windows.iter().any(|window| {
                !valid_text(&window.key, 256)
                    || window
                        .label
                        .as_deref()
                        .is_some_and(|label| !valid_text(label, 256))
                    || window
                        .group
                        .as_deref()
                        .is_some_and(|group| !valid_text(group, 128))
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
    Ok(())
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
