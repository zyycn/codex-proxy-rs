//! 推理响应携带的额度观察合并与 Provider 快照规范化

use super::*;

impl CodexCredentialQuotaService {
    /// 把正常推理响应携带的限流事实合并进 Provider 原始 quota JSON
    pub async fn synchronize_passive_headers(
        &self,
        account: &ProviderAccount,
        headers: &[(String, String)],
    ) -> Result<bool, CodexCredentialQuotaError> {
        let Some(rate_limits) = parse_rate_limit_headers(headers) else {
            return Ok(false);
        };
        self.synchronize_passive_rate_limits(account, std::slice::from_ref(&rate_limits))
            .await
    }

    /// 把一次推理响应中采集的结构化限流观察合并后单次落库
    pub async fn synchronize_passive_rate_limits(
        &self,
        account: &ProviderAccount,
        rate_limits: &[ParsedRateLimits],
    ) -> Result<bool, CodexCredentialQuotaError> {
        if account.authentication_kind() != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
            || rate_limits.is_empty()
        {
            return Ok(false);
        }
        let has_quota_facts = rate_limits.iter().any(|observation| {
            observation
                .limits
                .values()
                .any(|details| passive_rate_limit_snapshot(details).is_some())
        });
        let existing = self
            .store
            .get_quotas(std::slice::from_ref(account.id()))
            .await?
            .into_iter()
            .find(|observation| {
                observation.account_id == *account.id()
                    && observation.expected_revision == account.revision()
            });
        let existing_state = existing.as_ref().map(|observation| observation.state);
        let existing = existing
            .map(|observation| observation.quota.into_inner())
            .unwrap_or_default();
        // 只用本次响应明确携带的套餐更新账号，不能把合并前的旧快照重新当作新证据
        let observed_plan = rate_limits
            .iter()
            .rev()
            .filter_map(|observation| {
                observation.plan_type.as_deref().filter(|plan| {
                    !plan.trim().is_empty() && !plan.trim().eq_ignore_ascii_case("unknown")
                })
            })
            .next();
        let plan_type = observed_account_plan(account.plan_type(), observed_plan);
        if !has_quota_facts && existing_state.is_none() {
            return Ok(false);
        }
        let prior = existing_state.unwrap_or_else(|| account.quota());
        let (quota, updated_default) = merge_passive_quota(existing, rate_limits);
        let observed_at = SystemTime::now();
        let snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(quota.clone()),
        )?;
        let credit_access = prior.evidence() == Some(QuotaEvidence::CreditsAvailable);
        let fresh_credits = rate_limits.iter().any(|limits| limits.credits.is_some());
        // 具名桶和元数据不改变套餐结论；点数不能再用时，仍须撤销原先仅凭点数的放行
        let subscription = if !updated_default
            || (snapshot.quota().access() == QuotaAccessState::Unknown
                && (credit_access || prior.is_exhausted()))
        {
            if credit_access {
                QuotaState::exhausted(
                    QuotaEvidence::AccountLimitReached,
                    observed_at,
                    prior.reset_at(),
                )
            } else {
                prior
            }
        } else if snapshot.quota().is_exhausted() {
            snapshot.quota()
        } else {
            QuotaState::allowed(observed_at)
        };
        // 套餐刚耗尽时不能拿合并快照里的旧余额作为首次放行依据
        let state = snapshot.access_with_credits(
            subscription,
            account.auto_use_quota_credits()
                && (credit_access || (updated_default && fresh_credits)),
        );
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                expected_connection: None,
                plan_type,
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(quota),
                observed_at,
                state,
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            return Ok(false);
        }
        if has_quota_facts {
            self.scheduling.observe(&snapshot);
        }
        Ok(true)
    }
}

fn merge_passive_quota(
    mut quota: Map<String, Value>,
    observations: &[ParsedRateLimits],
) -> (Map<String, Value>, bool) {
    let mut updated_default = false;
    let mut snapshots = RateLimitSnapshotsByLimitId::take_from_document(&mut quota);
    for rate_limits in observations {
        let default_is_named_alias = default_limit_is_named_alias(rate_limits);
        let mut resolved_limit_ids = BTreeMap::new();
        for (wire_limit_id, details) in &rate_limits.limits {
            // HTTP 与 WebSocket 都可能把活动具名桶镜像为默认 `codex` 窗口
            // 同一 wire 观察里存在相同具名事实时丢弃镜像，不触碰 core 桶
            if wire_limit_id == DEFAULT_CODEX_LIMIT_ID && default_is_named_alias {
                continue;
            }
            let Some(limit_id) = snapshots.resolve_limit_id(details) else {
                continue;
            };
            let Some(rate_limit) = passive_rate_limit_snapshot(details) else {
                continue;
            };
            updated_default |= limit_id == DEFAULT_CODEX_LIMIT_ID;
            snapshots.upsert(&limit_id, details, rate_limit);
            resolved_limit_ids.insert(wire_limit_id.as_str(), limit_id);
        }

        let active_limit = rate_limits
            .active_limit
            .as_deref()
            .and_then(|wire_limit_id| {
                resolved_limit_ids.get(wire_limit_id).cloned().or_else(|| {
                    (!rate_limits.limits.contains_key(wire_limit_id))
                        .then(|| wire_limit_id.to_owned())
                })
            })
            .or_else(|| resolved_limit_ids.get(DEFAULT_CODEX_LIMIT_ID).cloned());
        if let Some(active_limit) = active_limit {
            quota.insert("active_limit".to_owned(), Value::String(active_limit));
        }
        merge_passive_metadata(&mut quota, rate_limits);
    }
    snapshots.write_to_document(&mut quota);
    (quota, updated_default)
}

fn merge_passive_metadata(quota: &mut Map<String, Value>, rate_limits: &ParsedRateLimits) {
    if let Some(plan_type) = rate_limits.plan_type.as_ref() {
        quota.insert("plan_type".to_owned(), Value::String(plan_type.clone()));
    }
    if let Some(promo_message) = rate_limits.promo_message.as_ref() {
        quota.insert(
            "promo_message".to_owned(),
            Value::String(promo_message.clone()),
        );
    }
    if let Some(reached_type) = rate_limits.rate_limit_reached_type.as_ref() {
        quota.insert(
            "rate_limit_reached_type".to_owned(),
            Value::String(reached_type.clone()),
        );
    }
    if let Some(credits) = rate_limits.credits.as_ref() {
        let mut value = quota
            .remove("credits")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        value.insert("has_credits".to_owned(), Value::Bool(credits.has_credits));
        value.insert("unlimited".to_owned(), Value::Bool(credits.unlimited));
        if let Some(balance) = credits.balance.as_ref() {
            value.insert("balance".to_owned(), Value::String(balance.clone()));
        } else {
            // 点数对象明确更新但未提供余额时，不能把旧余额继续作为当前余额展示
            value.remove("balance");
        }
        quota.insert("credits".to_owned(), Value::Object(value));
    }
}

fn default_limit_is_named_alias(rate_limits: &ParsedRateLimits) -> bool {
    let Some(default_primary) = rate_limits
        .limits
        .get(DEFAULT_CODEX_LIMIT_ID)
        .and_then(|details| details.primary)
    else {
        return false;
    };
    rate_limits.limits.iter().any(|(limit_id, details)| {
        limit_id != DEFAULT_CODEX_LIMIT_ID && details.primary == Some(default_primary)
    })
}

fn passive_rate_limit_snapshot(details: &RateLimitDetails) -> Option<Map<String, Value>> {
    // 响应头的窗口属于同一次上游观测；跨响应合并会制造不存在的额度窗口
    let mut snapshot = Map::new();
    if let Some(allowed) = details.allowed {
        snapshot.insert("allowed".to_owned(), Value::Bool(allowed));
    }
    if let Some(limit_reached) = details.limit_reached {
        snapshot.insert("limit_reached".to_owned(), Value::Bool(limit_reached));
    }
    for (field, window) in [
        ("primary_window", details.primary),
        ("secondary_window", details.secondary),
    ] {
        let Some(window) = window else {
            continue;
        };
        snapshot.insert(
            field.to_owned(),
            Value::Object(passive_rate_limit_window(window)),
        );
    }
    (!snapshot.is_empty()).then_some(snapshot)
}

fn passive_rate_limit_window(window: RateLimitWindow) -> Map<String, Value> {
    let mut snapshot = Map::new();
    if let Some(number) = serde_json::Number::from_f64(window.used_percent) {
        snapshot.insert("used_percent".to_owned(), Value::Number(number));
    }
    if let Some(seconds) = window
        .window_minutes
        .and_then(|minutes| minutes.checked_mul(60))
    {
        snapshot.insert("limit_window_seconds".to_owned(), Value::from(seconds));
    }
    if let Some(reset_at) = window.reset_at {
        snapshot.insert("reset_at".to_owned(), Value::from(reset_at));
    }
    snapshot
}
