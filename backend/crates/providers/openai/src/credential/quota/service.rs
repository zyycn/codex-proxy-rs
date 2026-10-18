//! 额度权威读取、刷新、错误反馈与服务组装

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotaRefreshAuthority {
    ObserveAccess,
    PreserveAccess,
}

struct FetchedCodexQuota {
    account: ProviderAccount,
    value: Value,
}

enum CodexQuotaFetchAttemptError {
    InvalidCredential,
    Upstream(CodexClientError),
}

enum CodexQuotaFetchError {
    InvalidCredential,
    Upstream {
        account: Box<ProviderAccount>,
        error: Box<CodexClientError>,
    },
}

/// 客户端错误携带的 HTTP 状态码；传输失败等为 `None`
fn upstream_error_status(error: &CodexClientError) -> Option<u16> {
    match error {
        CodexClientError::Upstream { status, .. } => Some(status.as_u16()),
        _ => None,
    }
}

/// 从上游错误体提取稳定错误码：优先 `/error/code`，其次 `/code`
///
/// 只接受有界的 ASCII 标识，避免把自由文本作为错误码写入诊断
fn upstream_error_code(error: &CodexClientError) -> Option<String> {
    let CodexClientError::Upstream { body, .. } = error else {
        return None;
    };
    let value = serde_json::from_str::<Value>(body).ok()?;
    let code = value
        .pointer("/error/code")
        .or_else(|| value.pointer("/code"))
        .and_then(Value::as_str)?
        .trim();
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(code.to_owned())
}

impl CodexCredentialQuotaService {
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: Client,
        base_url: String,
        cooldowns: Arc<dyn ProviderCooldownPort>,
        leases: Arc<dyn ProviderLeasePort>,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    ) -> Self {
        Self {
            store: Arc::clone(repository.store()),
            repository,
            profile,
            http,
            base_url,
            cooldowns,
            leases,
            runtime_policy,
            freeze_policy_cache: Mutex::new(None),
            scheduling: CodexQuotaSchedulingProjection::default(),
            reset_consume_locks: ResetCreditLocks::default(),
            initial_sync_delays: Arc::new(|| {
                crate::jitter::uniform_delay(
                    crate::jitter::random_u64(),
                    INITIAL_QUOTA_SYNC_MAX_START_DELAY,
                )
            }),
        }
    }

    /// 注入首查随机延迟采样器（测试用确定性节奏）；生产默认均匀
    /// `[0, INITIAL_QUOTA_SYNC_MAX_START_DELAY)`，零延迟等价旧行为
    #[doc(hidden)]
    pub fn with_initial_sync_delays(mut self, delays: CodexInitialSyncDelays) -> Self {
        self.initial_sync_delays = delays;
        self
    }

    /// 读取容量熔断策略（带短 TTL 缓存）；读取失败退化为关闭，熔断不得放大失败
    async fn freeze_policy(&self) -> ProviderFreezePolicy {
        {
            let cache = self.freeze_policy_cache.lock().await;
            if let Some((policy, loaded_at)) = cache.as_ref()
                && loaded_at.elapsed() < FREEZE_POLICY_CACHE_TTL
            {
                return policy.clone();
            }
        }
        let loaded = self.runtime_policy.load_freeze_policy().await.ok();
        let mut cache = self.freeze_policy_cache.lock().await;
        if let Some(policy) = loaded {
            *cache = Some((policy.clone(), Instant::now()));
            return policy;
        }
        cache
            .as_ref()
            .filter(|(_, loaded_at)| loaded_at.elapsed() < FREEZE_POLICY_CACHE_TTL)
            .map_or_else(ProviderFreezePolicy::disabled, |(policy, _)| policy.clone())
    }

    /// 容量熔断入口：滑动窗口内累计容量类失败，达到阈值即写入带
    /// `CapacityFreeze` 类别的账号级冷却
    /// 调度侧立即屏蔽该账号，恢复由
    /// freeze-recovery worker 处理；本路径只依赖 Redis 可丢失事实
    pub async fn apply_capacity_failure(&self, account: &ProviderAccount, observed_at: SystemTime) {
        let policy = self.freeze_policy().await;
        if !policy.enabled() {
            return;
        }
        let in_flight = self.current_in_flight(account.id()).await;
        let Ok(count) = self
            .cooldowns
            .record_capacity_failure(account.id(), policy.window(), in_flight)
            .await
        else {
            return;
        };
        if count < policy.threshold() {
            return;
        }
        let Some(until) = observed_at.checked_add(policy.freeze_duration()) else {
            return;
        };
        let cooldown = ProviderCooldown::new_with_kind(
            account.id().clone(),
            account.revision(),
            until,
            if policy.probe_enabled() {
                ProviderCooldownKind::CapacityFreezeProbe
            } else {
                ProviderCooldownKind::CapacityFreeze
            },
        );
        if self.cooldowns.put_if_later(cooldown).await.is_ok() {
            tracing::warn!(
                account_id = account.id().as_str(),
                threshold = policy.threshold(),
                window_seconds = policy.window().as_secs(),
                freeze_seconds = policy.freeze_duration().as_secs(),
                peak_in_flight = in_flight,
                "账号容量熔断触发：冻结该账号一段时间",
            );
        }
    }

    async fn current_in_flight(&self, account_id: &ProviderAccountId) -> u32 {
        self.leases
            .account_in_flight(std::slice::from_ref(account_id))
            .await
            .ok()
            .and_then(|signals| signals.get(account_id).copied())
            .unwrap_or(0)
    }

    /// 真实推理错误只更新额度访问事实，不伪造 Provider JSON 或展示百分比
    pub(crate) async fn record_confirmed_exhaustion(
        &self,
        account: &ProviderAccount,
        evidence: QuotaEvidence,
        reset_at: Option<SystemTime>,
        observed_at: SystemTime,
    ) -> Result<(), CodexCredentialQuotaError> {
        let outcome = self
            .store
            .apply_quota_access(QuotaAccessChange {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                state: QuotaState::exhausted(evidence, observed_at, reset_at),
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        Ok(())
    }

    /// 成功推理是额度可访问的权威证据，同时解除账号级 429 冷却
    pub async fn record_successful_inference(
        &self,
        account: &ProviderAccount,
        observed_at: SystemTime,
    ) -> Result<(), CodexCredentialQuotaError> {
        // 已经发出的并发请求可能晚于耗尽事实成功返回；真实成功仍是独立的
        // Allowed 权威证据，但调度不会为了试探恢复而放行耗尽账号
        if account.quota().access() == QuotaAccessState::Exhausted {
            let outcome = self
                .store
                .apply_quota_access(QuotaAccessChange {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    state: QuotaState::allowed(observed_at),
                })
                .await?;
            if outcome == QuotaWriteOutcome::Conflict {
                return Err(CodexCredentialQuotaError::RevisionConflict);
            }
        }
        self.cooldowns
            .clear_after_success(account.id(), account.revision())
            .await
            .map_err(|error| CodexCredentialQuotaError::Store {
                detail: error.to_string(),
            })?;
        Ok(())
    }

    pub async fn synchronize(&self) -> Result<CodexQuotaSyncSummary, CodexCredentialQuotaError> {
        self.synchronize_at(SystemTime::now()).await
    }

    /// 按本轮调度时刻选择到期账号；实际 HTTP 观察与落库仍使用发生时刻
    pub async fn synchronize_at(
        &self,
        now: SystemTime,
    ) -> Result<CodexQuotaSyncSummary, CodexCredentialQuotaError> {
        let mut accounts = self.repository.list_for_provider().await?;
        accounts.retain(|account| {
            account.authentication_kind() == crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        });
        let mut summary = CodexQuotaSyncSummary::default();
        let account_ids = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        let observed = self.store.get_quotas(&account_ids).await?;
        let observed_snapshots = observed
            .iter()
            .filter_map(|obs| {
                quota_snapshot_from_observation(obs)
                    .map(|snapshot| (obs.account_id.clone(), snapshot))
            })
            .collect::<BTreeMap<_, _>>();
        let observed_ids = observed
            .into_iter()
            .map(|observation| observation.account_id)
            .collect::<BTreeSet<_>>();
        let initial = self.scheduling.reserve_initial_refreshes(
            &accounts,
            &observed_ids,
            now,
            self.initial_sync_delays.as_ref(),
        );
        // 本轮首查后仍无快照的账号需要重排随机延迟。
        let mut pending_initial: BTreeSet<ProviderAccountId> =
            initial.iter().map(|account| account.id().clone()).collect();
        let periodic =
            self.scheduling
                .reserve_periodic_refreshes(accounts, &observed_snapshots, now);
        let accounts = initial
            .into_iter()
            .chain(periodic)
            .fold(BTreeMap::new(), |mut unique, account| {
                unique.insert(account.id().clone(), account);
                unique
            })
            .into_values()
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return Ok(summary);
        }
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        for account in accounts {
            let observed_at = SystemTime::now();
            match self.fetch_usage(&client, &account).await {
                Ok(FetchedCodexQuota { account, value }) => {
                    // 单账号解析或落库失败只影响该账号；其余账号继续同步
                    match self
                        .apply_fetched_quota(&account, &value, observed_at, &mut summary)
                        .await
                    {
                        Ok(()) => {
                            pending_initial.remove(account.id());
                        }
                        Err(error) => {
                            summary.transient += 1;
                            tracing::warn!(
                                account_id = %account.id(),
                                error = %error,
                                "OpenAI quota synchronization skipped one account"
                            );
                        }
                    }
                }
                Err(CodexQuotaFetchError::InvalidCredential) => {
                    summary.stale += 1;
                }
                Err(CodexQuotaFetchError::Upstream { account, error }) => {
                    match classify_quota_endpoint_failure(&error) {
                        Some(QuotaEndpointFailure::Exhausted(evidence)) => {
                            summary.exhausted += 1;
                            if let Err(write_error) = self
                                .record_confirmed_exhaustion(&account, evidence, None, observed_at)
                                .await
                            {
                                summary.transient += 1;
                                tracing::warn!(
                                    account_id = %account.id(),
                                    error = %write_error,
                                    "OpenAI quota exhaustion fact write failed"
                                );
                            }
                        }
                        Some(QuotaEndpointFailure::Credential { state, reason }) => {
                            summary.banned += 1;
                            self.persist_credential_failure(&account, state, reason, observed_at)
                                .await;
                        }
                        None => {
                            summary.transient += 1;
                            tracing::warn!(
                                account_id = %account.id(),
                                error = %error,
                                upstream_status = upstream_error_status(&error),
                                upstream_code = ?upstream_error_code(&error),
                                "OpenAI quota upstream rejection; refresh cycle will retry later"
                            );
                        }
                    }
                }
            }
        }
        // 首查失败或未能落库的账号重排随机延迟；已成功落库的账号由快照事实自然退出首查路径。
        self.scheduling
            .defer_failed_initial_refreshes(&pending_initial, self.initial_sync_delays.as_ref());
        Ok(summary)
    }

    #[must_use]
    pub fn runtime_policy(&self) -> &Arc<dyn ProviderRuntimePolicyPort> {
        &self.runtime_policy
    }

    /// 解析并 revision-fenced 落库单账号的 Provider quota JSON
    async fn apply_fetched_quota(
        &self,
        account: &ProviderAccount,
        value: &Value,
        observed_at: SystemTime,
        summary: &mut CodexQuotaSyncSummary,
    ) -> Result<(), CodexCredentialQuotaError> {
        let mut object = normalize_quota_window_placeholders(
            value
                .as_object()
                .cloned()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
        );
        object.remove(RECOVERY_FIELD);
        let mut snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(object.clone()),
        )?;
        let previous = if account.quota().is_exhausted() {
            self.read_snapshot_for(account).await?
        } else {
            None
        };
        let state = reconcile_refresh(
            account.quota(),
            &mut snapshot,
            previous.as_ref(),
            &mut object,
        )?;
        let snapshot = snapshot.with_quota_state(state);
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                plan_type: observed_account_plan(account.plan_type(), snapshot.plan_type()),
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(object),
                observed_at,
                state,
            })
            .await?;
        if outcome == QuotaWriteOutcome::Conflict {
            summary.stale += 1;
            return Ok(());
        }
        self.scheduling.observe(&snapshot);
        if snapshot.quota().is_exhausted() {
            summary.exhausted += 1;
        } else {
            summary.updated += 1;
        }
        Ok(())
    }

    /// 真实 429 的单一事实入口：写入 Redis 临时限流冷却（`until = now + retry_after`）
    /// 凭据与额度主窗口（额度重置时间）都不改变——临时限流是独立维度，
    /// 到期由 Redis key 过期自动解除，不污染配额耗尽状态
    /// 已有更晚的冷却不会被缩短（put_if_later）
    pub async fn apply_rate_limit_429(
        &self,
        account: &ProviderAccount,
        retry_after: Option<Duration>,
        observed_at: SystemTime,
    ) -> Result<(), CodexCredentialQuotaError> {
        let until = observed_at
            .checked_add(retry_after.unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN))
            .unwrap_or(observed_at);
        self.cooldowns
            .put_if_later(ProviderCooldown::new(
                account.id().clone(),
                account.revision(),
                until,
            ))
            .await
            .map_err(|error| CodexCredentialQuotaError::Store {
                detail: error.to_string(),
            })?;
        Ok(())
    }

    /// 读取有效的账号冷却事实；等待恢复探测的冻结到期后仍有效
    pub async fn cooldown(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<gateway_core::account::AccountCooldown>, CodexCredentialQuotaError> {
        let Some(cooldown) = self.cooldowns.read(account_id).await.map_err(|error| {
            CodexCredentialQuotaError::Store {
                detail: error.to_string(),
            }
        })?
        else {
            return Ok(None);
        };
        let state = cooldown.scheduling_state();
        Ok(state.is_active(SystemTime::now()).then_some(state))
    }

    /// 读取单账号最后一次落库的 Provider quota，并由 Codex 域解析展示窗口
    pub async fn read_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<CodexAccountQuotaSnapshot>, CodexCredentialQuotaError> {
        let account = self
            .store
            .get_account(account_id)
            .await?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexCredentialQuotaError::NotFound)?;
        self.read_snapshot_for(&account).await
    }

    async fn read_snapshot_for(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<CodexAccountQuotaSnapshot>, CodexCredentialQuotaError> {
        if account.provider().as_str() != "openai" {
            return Err(CodexCredentialQuotaError::NotFound);
        }
        let account_id = account.id();
        let Some(observation) = self
            .store
            .get_quotas(std::slice::from_ref(account_id))
            .await?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        if observation.account_id != *account_id
            || observation.expected_revision != account.revision()
        {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        if observation.quota.expose_to_provider().is_empty() {
            return Ok(None);
        }
        let observed_at = observation.observed_at;
        let snapshot = parse_account_quota_snapshot(
            account_id.clone(),
            account.revision(),
            observed_at,
            &Value::Object(observation.quota.expose_to_provider().clone()),
        )?
        .with_quota_state(observation.state);
        self.scheduling.observe(&snapshot);
        Ok(Some(snapshot))
    }

    /// 只刷新指定账号，revision-fenced 写入动态 Provider JSON 后返回解析快照
    pub async fn refresh_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        self.refresh_account_snapshot(account_id, QuotaRefreshAuthority::ObserveAccess)
            .await
    }

    /// 真实限额失败后的异步回读仅允许明确可用的点数恢复访问，套餐状态仍保留失败保护
    pub(crate) async fn refresh_account_after_failure(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        self.refresh_account_snapshot(account_id, QuotaRefreshAuthority::PreserveAccess)
            .await
    }

    async fn refresh_account_snapshot(
        &self,
        account_id: &ProviderAccountId,
        authority: QuotaRefreshAuthority,
    ) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
        let account = self
            .store
            .get_account(account_id)
            .await?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexCredentialQuotaError::NotFound)?;
        if account.authentication_kind() != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH {
            return Err(CodexCredentialQuotaError::NotFound);
        }
        let observed_at = SystemTime::now();
        if !access_token_is_current(&account, observed_at) {
            return Err(CodexCredentialQuotaError::CredentialRefreshRequired);
        }
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let FetchedCodexQuota { account, value } = match self.fetch_usage(&client, &account).await {
            Ok(fetched) => fetched,
            Err(CodexQuotaFetchError::InvalidCredential) => {
                return Err(CodexCredentialQuotaError::InvalidCredentialData);
            }
            Err(CodexQuotaFetchError::Upstream { account, error }) => {
                match classify_quota_endpoint_failure(&error) {
                    Some(QuotaEndpointFailure::Exhausted(evidence)) => {
                        self.record_confirmed_exhaustion(&account, evidence, None, observed_at)
                            .await?;
                    }
                    Some(QuotaEndpointFailure::Credential { state, reason }) => {
                        self.persist_credential_failure(&account, state, reason, observed_at)
                            .await;
                    }
                    None => {}
                }
                return Err(CodexCredentialQuotaError::Upstream {
                    detail: error.to_string(),
                    status: upstream_error_status(&error),
                    code: upstream_error_code(&error),
                });
            }
        };
        let mut object = normalize_quota_window_placeholders(
            value
                .as_object()
                .cloned()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
        );
        object.remove(RECOVERY_FIELD);
        let mut snapshot = parse_account_quota_snapshot(
            account.id().clone(),
            account.revision(),
            observed_at,
            &Value::Object(object.clone()),
        )?;
        let previous = if account.quota().is_exhausted() {
            self.read_snapshot_for(&account).await?
        } else {
            None
        };
        // 失败后回读只允许明确可用的点数恢复访问，套餐 allowed 仍可能滞后
        let observe_access =
            matches!(authority, QuotaRefreshAuthority::ObserveAccess) || snapshot.credits_usable();
        if account.quota().is_exhausted() && !observe_access {
            if self
                .store
                .touch_quota_observation(QuotaObservationTouch {
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    observed_at,
                })
                .await?
                == QuotaWriteOutcome::Conflict
            {
                return Err(CodexCredentialQuotaError::RevisionConflict);
            }
            return Ok(match previous {
                Some(previous) => {
                    let previous = previous.with_observed_at(observed_at);
                    self.scheduling.observe(&previous);
                    previous
                }
                None => snapshot
                    .with_quota_state(account.quota())
                    .with_observed_at(observed_at),
            });
        }
        let state = if observe_access {
            reconcile_refresh(
                account.quota(),
                &mut snapshot,
                previous.as_ref(),
                &mut object,
            )?
        } else {
            account.quota()
        };
        let snapshot = snapshot.with_quota_state(state);
        if self
            .store
            .compare_and_swap_quota(QuotaObservation {
                plan_type: observed_account_plan(account.plan_type(), snapshot.plan_type()),
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(object),
                observed_at,
                state,
            })
            .await?
            == QuotaWriteOutcome::Conflict
        {
            return Err(CodexCredentialQuotaError::RevisionConflict);
        }
        self.scheduling.observe(&snapshot);
        Ok(snapshot)
    }

    async fn persist_credential_failure(
        &self,
        account: &ProviderAccount,
        credential_state: CredentialState,
        reason: AccountErrorReason,
        observed_at: SystemTime,
    ) {
        if let Err(error) = self
            .repository
            .apply_state_with_reason(account, credential_state, observed_at, Some(reason), None)
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                ?credential_state,
                reason = reason.as_str(),
                error = %error,
                "OpenAI quota credential fact write failed"
            );
        }
    }

    async fn fetch_usage(
        &self,
        client: &CodexBackendClient,
        account: &ProviderAccount,
    ) -> Result<FetchedCodexQuota, CodexQuotaFetchError> {
        let credential = self
            .repository
            .load_runtime_credential(account)
            .await
            .map_err(|_| CodexQuotaFetchError::InvalidCredential)?;
        let prepared = PreparedCodexRuntimeCredential {
            account: account.clone(),
            credential,
        };
        let result = fetch_usage_with_5xx_retry(client, &prepared).await;
        match result {
            Ok(value) => Ok(FetchedCodexQuota {
                account: prepared.account,
                value,
            }),
            Err(CodexQuotaFetchAttemptError::InvalidCredential) => {
                Err(CodexQuotaFetchError::InvalidCredential)
            }
            Err(CodexQuotaFetchAttemptError::Upstream(error)) => {
                Err(CodexQuotaFetchError::Upstream {
                    account: Box::new(prepared.account),
                    error: Box::new(error),
                })
            }
        }
    }
}

async fn fetch_usage_once(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
) -> Result<Value, CodexQuotaFetchAttemptError> {
    let authorization = prepared
        .credential
        .authentication
        .authorization_header()
        .map_err(|_| CodexQuotaFetchAttemptError::InvalidCredential)?;
    let request_id = format!("quota_{}", Uuid::now_v7().simple());
    client
        .for_account(&prepared.account)
        .map_err(CodexQuotaFetchAttemptError::Upstream)?
        .fetch_usage(CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            prepared.account.upstream_account_id(),
            &request_id,
            None,
        ))
        .await
        .map_err(CodexQuotaFetchAttemptError::Upstream)
}

/// 对 5xx 上游拒绝做有限次指数退避重试（1s/2s），吞掉瞬时抖动
///
/// 4xx（含 402/429）不重试：它们已经走额度状态转换，重试只会放大上游负载
async fn fetch_usage_with_5xx_retry(
    client: &CodexBackendClient,
    prepared: &PreparedCodexRuntimeCredential,
) -> Result<Value, CodexQuotaFetchAttemptError> {
    let mut attempt = 0_u32;
    loop {
        let result = fetch_usage_once(client, prepared).await;
        let retryable = match &result {
            Ok(_) => false,
            Err(CodexQuotaFetchAttemptError::Upstream(CodexClientError::Upstream {
                status,
                ..
            })) => status.is_server_error(),
            Err(_) => false,
        };
        if !retryable || attempt >= QUOTA_FETCH_5XX_MAX_RETRIES {
            return result;
        }
        attempt += 1;
        let delay = QUOTA_FETCH_5XX_BASE_DELAY.saturating_mul(attempt);
        tracing::warn!(
            account_id = %prepared.account.id(),
            retry_attempt = attempt,
            retry_delay_ms = delay.as_millis(),
            "OpenAI quota usage 5xx upstream rejection; retrying with backoff"
        );
        tokio::time::sleep(delay).await;
    }
}
