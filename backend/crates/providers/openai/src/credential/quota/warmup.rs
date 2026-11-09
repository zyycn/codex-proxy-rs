//! 账号额度窗口预热与完成事件校验

use super::*;

// 上游倒计时按整秒返回，跨过一个完整秒后再区分刚激活与空闲满窗口
const WINDOW_CONFIRM_DELAY: Duration = Duration::from_secs(2);

enum WarmupDecision<'a> {
    Ready,
    Active(&'a CodexQuotaWindow),
    Exhausted(Option<&'a CodexQuotaWindow>),
    Verify(&'a CodexQuotaWindow),
}

fn warmup_account_is_available(account: &ProviderAccount) -> bool {
    account.authentication_kind() == crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        && account.enabled()
        && matches!(
            account.credential_state(),
            CredentialState::Unknown | CredentialState::Ready
        )
}

fn warmup_decision<'a>(
    snapshot: &'a CodexAccountQuotaSnapshot,
    previous: Option<&CodexAccountQuotaSnapshot>,
    now: chrono::DateTime<chrono::Utc>,
) -> WarmupDecision<'a> {
    if snapshot.quota().is_exhausted() {
        return WarmupDecision::Exhausted(None);
    }
    if let Some(window) = snapshot.windows().iter().find(|window| {
        window.is_account_wide()
            && window.kind() == CodexQuotaWindowKind::Weekly
            && window.limit_reached()
    }) {
        return WarmupDecision::Exhausted(Some(window));
    }

    let mut uncertain = None;
    for window in snapshot.windows().iter().filter(|window| {
        window.is_account_wide()
            && window.kind() == CodexQuotaWindowKind::ShortTerm
            && window
                .reset_at()
                .is_some_and(|reset| reset > now + chrono::Duration::minutes(30))
    }) {
        // 0% 可能只是取整；已经开始递减的原始倒计时仍能证明窗口已激活
        if window.used_percent().is_some_and(|used| used > 0.0)
            || window
                .reset_after_seconds()
                .zip(window.window_seconds())
                .is_some_and(|(remaining, total)| remaining > 0 && remaining < total)
        {
            return WarmupDecision::Active(window);
        }
        // 连续两次实时查询仍返回完整窗口且 reset 向后移动，才确认空闲
        // 不以快照级 observed_at 推算，其他额度桶的被动更新也会推进该时间
        let confirmed_idle = idle_full_window(window)
            && previous.is_some_and(|previous| {
                previous.windows().iter().any(|prior| {
                    prior.key() == window.key()
                        && idle_full_window(prior)
                        && prior
                            .reset_at()
                            .zip(window.reset_at())
                            .is_some_and(|(before, after)| after > before)
                })
            });
        if !confirmed_idle {
            uncertain = Some(window);
        }
    }
    uncertain.map_or(WarmupDecision::Ready, WarmupDecision::Verify)
}

fn idle_full_window(window: &CodexQuotaWindow) -> bool {
    window.used_percent() == Some(0.0)
        && window
            .reset_after_seconds()
            .zip(window.window_seconds())
            .is_some_and(|(remaining, total)| remaining == total)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WarmupTerminal {
    Missing,
    Completed,
    Failed,
}

fn observe_warmup_events(events: Vec<SseEvent>, terminal: &mut WarmupTerminal) {
    for event in events {
        let parsed = serde_json::from_str::<Value>(&event.data).ok();
        let kind = parsed
            .as_ref()
            .and_then(|data| data.get("type"))
            .and_then(Value::as_str)
            .or(event.event.as_deref());
        match kind {
            Some("response.completed") => {
                // 只有明确完成的响应才算预热成功；失败事件即使随后出现完成帧也优先
                if *terminal != WarmupTerminal::Failed {
                    *terminal = if parsed.is_some()
                        && parsed
                            .as_ref()
                            .and_then(|data| data.pointer("/response/status"))
                            .and_then(Value::as_str)
                            .is_none_or(|status| status == "completed")
                    {
                        WarmupTerminal::Completed
                    } else {
                        WarmupTerminal::Failed
                    };
                }
            }
            Some("response.failed" | "response.incomplete" | "error") => {
                *terminal = WarmupTerminal::Failed;
            }
            _ => {}
        }
    }
}

async fn consume_warmup_sse(
    response: &mut CodexBackendStreamingResponse,
) -> Result<(), &'static str> {
    let mut decoder = SseEventDecoder::default();
    let mut terminal = WarmupTerminal::Missing;
    let mut received_bytes = 0usize;
    while let Some(chunk) = response.body.next().await {
        let chunk = chunk.map_err(|_| "stream_read_failed")?;
        received_bytes = received_bytes.saturating_add(chunk.len());
        if received_bytes > WARMUP_STREAM_MAX_BYTES {
            return Err("stream_size_limit_exceeded");
        }
        let events = decoder.push(&chunk).map_err(|_| "invalid_sse")?;
        observe_warmup_events(events, &mut terminal);
        if terminal != WarmupTerminal::Missing {
            break;
        }
    }
    if terminal == WarmupTerminal::Missing {
        observe_warmup_events(decoder.finish().map_err(|_| "invalid_sse")?, &mut terminal);
    }
    match terminal {
        WarmupTerminal::Completed => Ok(()),
        WarmupTerminal::Missing => Err("completion_event_missing"),
        WarmupTerminal::Failed => Err("response_failed"),
    }
}

impl CodexCredentialQuotaService {
    /// 批量预激活 OAuth 账号的 5h 配额滑动窗口
    pub async fn execute_warmup(
        &self,
        model: &str,
    ) -> Result<CodexWarmupSummary, CodexCredentialQuotaError> {
        let mut accounts = self.repository.list_for_provider().await?;
        accounts.retain(warmup_account_is_available);
        let mut summary = CodexWarmupSummary::default();
        if accounts.is_empty() {
            return Ok(summary);
        }
        let account_ids = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        let observed = self.store.get_quotas(&account_ids).await?;
        let mut observed_snapshots = observed
            .iter()
            .filter_map(|obs| {
                quota_snapshot_from_observation(obs)
                    .map(|snapshot| (obs.account_id.clone(), snapshot))
            })
            .collect::<BTreeMap<_, _>>();

        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );

        'accounts: for account in accounts {
            if account.quota().is_exhausted() {
                summary.skipped_exhausted += 1;
                tracing::info!(
                    account_id = %account.id(),
                    observed_at = ?account.quota().observed_at(),
                    reset_at = ?account.quota().reset_at(),
                    reason = "quota_exhausted",
                    "OpenAI account warmup skipped"
                );
                continue;
            }
            let mut snapshot = observed_snapshots
                .remove(account.id())
                .filter(|snapshot| snapshot.credential_revision() == account.revision());
            let mut previous = None;
            let mut refreshes = 0;
            while let Some(current) = snapshot.as_ref() {
                let (window, reason) =
                    match warmup_decision(current, previous.as_ref(), chrono::Utc::now()) {
                        WarmupDecision::Ready => break,
                        WarmupDecision::Active(window) => {
                            summary.skipped_active += 1;
                            (Some(window), "active_short_window")
                        }
                        WarmupDecision::Exhausted(window) => {
                            summary.skipped_exhausted += 1;
                            (window, "quota_exhausted")
                        }
                        WarmupDecision::Verify(window) if refreshes == 2 => {
                            summary.failed += 1;
                            (Some(window), "window_activity_unconfirmed")
                        }
                        WarmupDecision::Verify(_) => {
                            // 持久化快照不作为两次实时查询的首份证据
                            if refreshes == 1 {
                                previous = snapshot.take();
                                tokio::time::sleep(WINDOW_CONFIRM_DELAY).await;
                            }
                            match tokio::time::timeout(
                                WARMUP_REQUEST_TIMEOUT,
                                self.refresh_account(account.id()),
                            )
                            .await
                            {
                                Ok(Ok(refreshed))
                                    if refreshed.credential_revision() == account.revision() =>
                                {
                                    snapshot = Some(refreshed);
                                    refreshes += 1;
                                    continue;
                                }
                                _ => {
                                    summary.failed += 1;
                                    tracing::warn!(
                                        account_id = %account.id(),
                                        reason = "quota_refresh_failed",
                                        "OpenAI account warmup quota verification failed"
                                    );
                                    continue 'accounts;
                                }
                            }
                        }
                    };
                tracing::info!(
                    account_id = %account.id(),
                    observed_at = ?chrono::DateTime::<chrono::Utc>::from(current.observed_at()),
                    window = window.map(CodexQuotaWindow::key),
                    window_seconds = window.and_then(CodexQuotaWindow::window_seconds),
                    used_percent = window.and_then(CodexQuotaWindow::used_percent),
                    reset_at = ?window.and_then(CodexQuotaWindow::reset_at),
                    reset_after_seconds = window.and_then(CodexQuotaWindow::reset_after_seconds),
                    reason,
                    "OpenAI account warmup skipped"
                );
                continue 'accounts;
            }

            // 回查会更新额度与套餐；凭据加载仍核对最新账号的完整快照
            let account = if refreshes > 0 {
                match self.store.get_account(account.id()).await {
                    Ok(Some(current))
                        if current.provider() == account.provider()
                            && current.revision() == account.revision()
                            && warmup_account_is_available(&current)
                            && !current.quota().is_exhausted() =>
                    {
                        current
                    }
                    _ => {
                        summary.failed += 1;
                        tracing::warn!(
                            account_id = %account.id(),
                            reason = "account_changed_after_verification",
                            "OpenAI account warmup skipped"
                        );
                        continue;
                    }
                }
            } else {
                account
            };
            let credential = match self.repository.load_runtime_credential(&account).await {
                Ok(cred) => cred,
                Err(_) => {
                    summary.failed += 1;
                    continue;
                }
            };
            let authorization = match credential.authentication.authorization_header() {
                Ok(auth) => auth,
                Err(_) => {
                    summary.failed += 1;
                    continue;
                }
            };

            let mut body = Map::new();
            body.insert("model".to_owned(), Value::String(model.to_owned()));
            body.insert(
                "input".to_owned(),
                serde_json::json!([{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}]
                }]),
            );
            body.insert("stream".to_owned(), Value::Bool(true));
            body.insert("store".to_owned(), Value::Bool(false));
            body.insert(
                "service_tier".to_owned(),
                Value::String("default".to_owned()),
            );
            // 目录里各模型的最低档都是 low，none 会收到上游 error 终态并让整次预热记为失败
            body.insert("reasoning".to_owned(), serde_json::json!({"effort": "low"}));
            body.insert("text".to_owned(), serde_json::json!({"verbosity": "low"}));

            let upstream_request = CodexResponsesRequest::from_body(body);
            let request_id = format!("warmup_{}", Uuid::now_v7().simple());
            let client_for_account = match client.for_account(&account) {
                Ok(c) => c,
                Err(error) => {
                    tracing::warn!(account_id = %account.id(), error = %error, "warmup client for account failed");
                    summary.failed += 1;
                    continue;
                }
            };
            let context = crate::transport::CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                account.upstream_account_id(),
                &request_id,
                None,
            );

            // HTTP 200 只确认响应头；必须消费 SSE 直到终态才能确认预热结果和额度事件
            let attempt = tokio::time::timeout(WARMUP_REQUEST_TIMEOUT, async {
                let mut response = client_for_account
                    .create_response_stream_http_sse(&upstream_request, context)
                    .await
                    .map_err(|_| "request_rejected")?;
                let terminal = consume_warmup_sse(&mut response).await;
                let rate_limit_updates = match response.rate_limit_updates.as_ref() {
                    Some(updates) => std::mem::take(&mut *updates.lock().await),
                    None => Vec::new(),
                };
                Ok::<_, &'static str>((terminal, response.rate_limit_headers, rate_limit_updates))
            })
            .await;
            match attempt {
                Ok(Ok((terminal, headers, rate_limit_updates))) => {
                    match terminal {
                        Ok(()) => {
                            // 被动额度同步会把成功请求的窗口事实标为可用，失败流不能复用该结论
                            if !headers.is_empty()
                                && let Err(error) =
                                    self.synchronize_passive_headers(&account, &headers).await
                            {
                                tracing::warn!(account_id = %account.id(), error = %error, "OpenAI warmup rate-limit header sync failed");
                            }
                            if !rate_limit_updates.is_empty()
                                && let Err(error) = self
                                    .synchronize_passive_rate_limits(&account, &rate_limit_updates)
                                    .await
                            {
                                tracing::warn!(account_id = %account.id(), error = %error, "OpenAI warmup rate-limit event sync failed");
                            }
                            summary.warmed_up += 1;
                            tracing::info!(
                                account_id = %account.id(),
                                model,
                                "OpenAI account warmed up successfully"
                            );
                        }
                        Err(reason) => {
                            summary.failed += 1;
                            tracing::warn!(
                                account_id = %account.id(),
                                reason,
                                "OpenAI account warmup stream failed"
                            );
                        }
                    }
                }
                Ok(Err(reason)) => {
                    summary.failed += 1;
                    tracing::warn!(
                        account_id = %account.id(),
                        reason,
                        "OpenAI account warmup request rejected by upstream"
                    );
                }
                Err(_) => {
                    summary.failed += 1;
                    tracing::warn!(
                        account_id = %account.id(),
                        "OpenAI account warmup request timed out"
                    );
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        Ok(summary)
    }
}
