//! 套餐、独立额度桶及模型额度的持久事实与公共投影

use crate::transport::account::{ModelQuota, QuotaSummary, Tier};
use chrono::{DateTime, Utc};
use gateway_admin::model::provider_credentials::{
    ProviderQuota, ProviderQuotaWindow, QuotaLocalUsageAttribution,
};
use gateway_core::account::{AccountQuotaSignals, QuotaEvidence, QuotaState};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

#[derive(Serialize, Deserialize)]
pub(super) struct Document {
    pub(super) tier: Option<Tier>,
    pub(super) summary: QuotaSummary,
    pub(super) models: BTreeMap<String, ModelQuota>,
}

impl ModelQuota {
    pub(crate) fn remaining(&self) -> Option<f64> {
        self.remaining_fraction
            .as_ref()
            .and_then(serde_json::Number::as_f64)
            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
    }

    fn current(&self, now: SystemTime) -> bool {
        self.reset_time
            .is_none_or(|reset| SystemTime::from(reset) > now)
    }
}

impl Document {
    pub(super) fn model_observation(
        &self,
        model: &str,
        observed_at: SystemTime,
        now: SystemTime,
    ) -> Option<gateway_admin::ports::provider::ProviderPeerModelQuota> {
        let quota = self.models.get(model)?;
        let valid_until = observed_at.checked_add(Duration::from_secs(600))?;
        let valid_until = quota
            .reset_time
            .map(SystemTime::from)
            .map_or(valid_until, |reset| reset.min(valid_until));
        if observed_at > now || valid_until <= now {
            return None;
        }
        let state = if quota.remaining()? == 0.0 {
            QuotaState::exhausted(
                QuotaEvidence::UsageLimitReached,
                observed_at,
                quota.reset_time.map(SystemTime::from),
            )
        } else {
            QuotaState::allowed(observed_at)
        };
        Some(gateway_admin::ports::provider::ProviderPeerModelQuota { state, valid_until })
    }
    pub(super) fn plan_type(&self) -> Option<String> {
        self.tier
            .as_ref()
            .map(|tier| tier.id.trim())
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    }

    pub(super) fn access(&self, now: SystemTime) -> QuotaState {
        // 独立模型桶不能直接升级为整个账号耗尽，只有全部已知模型都明确耗尽才阻断账号
        if self
            .models
            .values()
            .any(|quota| quota.current(now) && quota.remaining().is_some_and(|v| v > 0.0))
        {
            QuotaState::allowed(now)
        } else if !self.models.is_empty()
            && self
                .models
                .values()
                .all(|quota| quota.current(now) && quota.remaining() == Some(0.0))
        {
            QuotaState::exhausted(
                QuotaEvidence::UsageLimitReached,
                now,
                self.models
                    .values()
                    .filter_map(|q| q.reset_time.map(SystemTime::from))
                    .min(),
            )
        } else {
            QuotaState::observed_unknown(now)
        }
    }

    pub(super) fn model_signals(
        &self,
        model: &str,
        observed: SystemTime,
        now: SystemTime,
    ) -> (bool, Option<AccountQuotaSignals>) {
        // 过时或已过重置点的百分比交回上游判定，避免旧快照永久挡住可用模型
        if now.duration_since(observed).unwrap_or_default() > Duration::from_secs(600) {
            return (false, None);
        }
        let Some(quota) = self.models.get(model).filter(|q| q.current(now)) else {
            return (false, None);
        };
        let remaining = quota.remaining();
        let reset = quota.reset_time.map(SystemTime::from);
        (
            remaining == Some(0.0),
            Some(AccountQuotaSignals::new(
                reset,
                remaining.map(|v| (v * 10_000.0).round() as u64),
            )),
        )
    }

    pub(super) fn project(&self, observed: SystemTime) -> ProviderQuota {
        let mut windows = Vec::new();
        for group in &self.summary.groups {
            let group_id = group.buckets.first().map(|b| {
                b.bucket_id
                    .strip_suffix(&format!("-{}", b.window))
                    .unwrap_or(&b.bucket_id)
                    .to_owned()
            });
            for bucket in &group.buckets {
                let (seconds, label, kind) = match bucket.window.as_str() {
                    "5h" => (Some(18_000), "5 小时额度", "shortTerm"),
                    "weekly" => (Some(604_800), "周额度", "shortTerm"),
                    _ => (None, bucket.display_name.as_str(), "other"),
                };
                let used = bucket.quota.remaining().map(|v| (1.0 - v) * 100.0);
                windows.push(ProviderQuotaWindow {
                    upstream: None,
                    key: bucket.bucket_id.clone(),
                    group: kind.into(),
                    label: label.into(),
                    limit_id: group_id.clone(),
                    limit_name: Some(group.display_name.clone()),
                    role: None,
                    local_usage_attribution: QuotaLocalUsageAttribution::Unavailable,
                    window_seconds: seconds,
                    used_percent: used,
                    reset_at: bucket.quota.reset_time,
                    limit_reached: used == Some(100.0),
                    local_usage: None,
                    provider_data: None,
                });
            }
        }
        ProviderQuota {
            limit_reached: self.access(observed).is_exhausted(),
            plan_type: self.plan_type(),
            observed_at: Some(DateTime::<Utc>::from(observed)),
            windows,
            ..Default::default()
        }
    }
}
