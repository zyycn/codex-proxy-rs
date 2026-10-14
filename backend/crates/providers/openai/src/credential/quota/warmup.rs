//! 账号额度窗口预热与完成事件校验

use super::*;

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
        accounts.retain(|account| {
            account.authentication_kind() == crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
                && account.enabled()
                && matches!(
                    account.credential_state(),
                    CredentialState::Unknown | CredentialState::Ready
                )
        });
        let mut summary = CodexWarmupSummary::default();
        if accounts.is_empty() {
            return Ok(summary);
        }
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

        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );

        let now_utc = chrono::Utc::now();
        for account in accounts {
            if let Some(snapshot) = observed_snapshots.get(account.id()) {
                // 1. 周线已触顶或耗尽跳过
                let weekly_exhausted = snapshot
                    .windows()
                    .iter()
                    .any(|w| w.kind() == CodexQuotaWindowKind::Weekly && w.limit_reached());
                if weekly_exhausted {
                    summary.skipped_exhausted += 1;
                    continue;
                }
                // 2. 5h 窗口当前活跃且距重置时间 > 30 分钟跳过
                let has_active_5h = snapshot.windows().iter().any(|w| {
                    w.kind() == CodexQuotaWindowKind::ShortTerm
                        && w.reset_at().is_some_and(|reset_at| {
                            reset_at > now_utc + chrono::Duration::minutes(30)
                        })
                });
                if has_active_5h {
                    summary.skipped_active += 1;
                    continue;
                }
            }

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
