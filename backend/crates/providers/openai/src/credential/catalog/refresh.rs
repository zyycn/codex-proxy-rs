//! 账号目录的批量恢复、变更通知和限速刷新

use super::*;

impl CodexCredentialCatalogService {
    /// 启动只恢复已成功落库、仍匹配账号身份的目录
    pub async fn restore(&self) -> Result<(), CodexCredentialCatalogError> {
        let revision = self.state_revision()?;
        let provider =
            ProviderKind::new("openai").map_err(|_| CodexCredentialCatalogError::Cache)?;
        let groups = self
            .repository
            .store()
            .load_account_catalogs(&provider)
            .await
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        let mut entries = Entries::new();
        for group in groups {
            let decoded =
                match crate::transport::CodexModelCatalogSnapshot::restore(&group.document) {
                    Ok(decoded) => decoded,
                    Err(error) => {
                        tracing::warn!(%error,"OpenAI stored model catalog is invalid");
                        continue;
                    }
                };
            let (models, _, _) = decoded.into_parts();
            let content = Arc::new(CatalogContent::new(models, &group.document));
            for a in group.accounts {
                let scope = CodexCatalogScope {
                    account_id: a.account_id,
                    revision: a.revision,
                    plan: a
                        .plan
                        .map(|p| p.trim().to_ascii_lowercase())
                        .filter(|p| !p.is_empty()),
                };
                entries.insert(
                    scope,
                    AccountEntry {
                        observed_at: a.observed_at,
                        content: content.clone(),
                        etag: a.etag,
                    },
                );
            }
        }
        let _writer = self
            .publication
            .lock()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        self.publish(revision, entries)?;
        self.cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .restored = true;
        Ok(())
    }

    pub(super) fn state_revision(&self) -> Result<u64, CodexCredentialCatalogError> {
        self.cache
            .read()
            .map(|c| c.revision)
            .map_err(|_| CodexCredentialCatalogError::Cache)
    }

    pub async fn invalidate_accounts(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<(), CodexCredentialCatalogError> {
        let ids = accounts.iter().collect::<BTreeSet<_>>();
        let current = self
            .repository
            .list_for_accounts(accounts)
            .await
            .inspect_err(|_| self.schedule_refresh())?;
        let current = current
            .iter()
            .filter(|a| eligible_catalog_account(a, SystemTime::now()))
            .map(CodexCatalogScope::for_account)
            .collect::<BTreeSet<_>>();
        let _writer = self
            .publication
            .lock()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        let revision = {
            let mut cache = self
                .cache
                .write()
                .map_err(|_| CodexCredentialCatalogError::Cache)?;
            cache.revision = cache.revision.wrapping_add(1);
            cache
                .pending
                .retain(|scope, _| !ids.contains(&scope.account_id) || current.contains(scope));
            cache.revision
        };
        if let Some(snapshot) = self.cached()? {
            let keep = |scope: &CodexCatalogScope| {
                !ids.contains(&scope.account_id) || current.contains(scope)
            };
            if snapshot.entries.keys().any(|s| !keep(s)) {
                let mut entries = snapshot.entries.clone();
                entries.retain(|s, _| keep(s));
                self.publish(revision, entries)?;
            }
        }
        self.schedule_refresh();
        Ok(())
    }

    pub fn observe_response_etag(
        &self,
        account: &ProviderAccount,
        etag: &str,
    ) -> Result<bool, CodexCredentialCatalogError> {
        validate_response_etag(etag)?;
        let scope = CodexCatalogScope::for_account(account);
        let mut state = self
            .cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if state.pending.get(&scope).map(String::as_str) == Some(etag)
            || !state.pending.contains_key(&scope)
                && state
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.entries.get(&scope))
                    .and_then(|e| e.etag.as_deref())
                    == Some(etag)
        {
            return Ok(false);
        }
        state.pending.insert(scope, etag.to_owned());
        drop(state);
        self.refresh_notification.notify_one();
        Ok(true)
    }

    pub(crate) fn schedule_refresh(&self) {
        self.refresh_notification.notify_one();
    }

    pub async fn wait_for_refresh(&self) {
        self.refresh_notification.notified().await;
    }

    /// 只合并成功的账号；手动刷新和后台任务同时完成时，保留较新的观测
    pub(super) fn merge_observations(
        &self,
        revision: u64,
        batch: Vec<(CodexCatalogScope, AccountEntry)>,
    ) -> Result<(), CodexCredentialCatalogError> {
        let _writer = self
            .publication
            .lock()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if self.state_revision()? != revision {
            return Err(CodexCredentialCatalogError::ConcurrentUpdate);
        }
        let mut entries = self
            .cached()?
            .map(|snapshot| snapshot.entries.clone())
            .unwrap_or_default();
        for (scope, entry) in batch {
            if entries
                .get(&scope)
                .is_none_or(|old| old.observed_at < entry.observed_at)
            {
                entries.insert(scope, entry);
            }
        }
        self.publish(revision, entries)?;
        Ok(())
    }

    fn publish(
        &self,
        revision: u64,
        entries: Entries,
    ) -> Result<Option<Arc<CodexCredentialCatalogSnapshot>>, CodexCredentialCatalogError> {
        let next =
            (!entries.is_empty()).then(|| Arc::new(CodexCredentialCatalogSnapshot::new(entries)));
        let old = self.cached()?;
        let changed = match (&old, &next) {
            (Some(a), Some(b)) => {
                a.entries.len() != b.entries.len()
                    || a.entries.iter().zip(&b.entries).any(|((sa, a), (sb, b))| {
                        sa != sb
                            || (!Arc::ptr_eq(&a.content, &b.content)
                                && a.content.digest != b.content.digest)
                    })
            }
            _ => old.is_some() != next.is_some(),
        };
        let mut cache = self
            .cache
            .write()
            .map_err(|_| CodexCredentialCatalogError::Cache)?;
        if cache.revision != revision {
            return Err(CodexCredentialCatalogError::ConcurrentUpdate);
        }
        if let Some(snapshot) = &next {
            cache.pending.retain(|scope, tag| {
                let Some(entry) = snapshot.entries.get(scope) else {
                    return true;
                };
                // 发布其他账号时保留通知，只有该账号更新的观测才能确认 ETag
                entry.etag.as_deref() != Some(tag)
                    || old
                        .as_ref()
                        .and_then(|s| s.entries.get(scope))
                        .is_some_and(|previous| previous.observed_at >= entry.observed_at)
            });
        }
        cache.snapshot = next.clone();
        if changed {
            cache.generation = cache.generation.wrapping_add(1);
        }
        Ok(next)
    }

    /// 一次对账只探测缺失、过期或有新 ETag 的账号；成功逐批发布，失败不更新观测时间
    pub async fn refresh_catalogs(
        &self,
    ) -> Result<Arc<CodexCredentialCatalogSnapshot>, CodexCredentialCatalogError> {
        let _writer = self.refresh_lock.lock().await;
        // 启动时数据库暂不可用，应先重试恢复，不能把整池误判为没有历史
        if !self
            .cache
            .read()
            .map_err(|_| CodexCredentialCatalogError::Cache)?
            .restored
        {
            self.restore().await?;
        }
        let revision = self.state_revision()?;
        let now = SystemTime::now();
        let accounts = self.repository.list_for_provider().await?;
        let current: BTreeMap<_, _> = accounts
            .into_iter()
            .filter(|a| eligible_catalog_account(a, now))
            .map(|a| (CodexCatalogScope::for_account(&a), a))
            .collect();
        let pending = {
            let mut cache = self
                .cache
                .write()
                .map_err(|_| CodexCredentialCatalogError::Cache)?;
            cache.pending.retain(|scope, _| current.contains_key(scope));
            cache.pending.clone()
        };
        let previous = self.cached()?;
        let mut entries = previous
            .as_ref()
            .map(|s| s.entries.clone())
            .unwrap_or_default();
        entries.retain(|scope, _| current.contains_key(scope));
        if previous
            .as_ref()
            .is_some_and(|s| s.entries.len() != entries.len())
        {
            let _writer = self
                .publication
                .lock()
                .map_err(|_| CodexCredentialCatalogError::Cache)?;
            let mut latest = self
                .cached()?
                .map(|s| s.entries.clone())
                .unwrap_or_default();
            latest.retain(|scope, _| current.contains_key(scope));
            self.publish(revision, latest)?;
        }
        let mut contents: BTreeMap<_, _> = entries
            .values()
            .map(|e| (e.content.digest, e.content.clone()))
            .collect();
        let due = current
            .into_iter()
            .filter(|(scope, _)| {
                pending.contains_key(scope)
                    || !entries.get(scope).is_some_and(|e| {
                        now.duration_since(e.observed_at)
                            .is_ok_and(|age| age < CATALOG_REFRESH_AFTER)
                    })
            })
            .collect::<Vec<_>>();
        if due.is_empty() {
            return self
                .cached()?
                .ok_or(CodexCredentialCatalogError::NoEligibleCredential);
        }
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        );
        let mut pace = tokio::time::interval(REFRESH_SPACING);
        pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let starts = stream::unfold(pace, |mut clock| async move {
            clock.tick().await;
            Some(((), clock))
        });
        let fetches = stream::iter(due)
            .zip(starts)
            .map(|((scope, account), ())| {
                let client = &client;
                let etag = pending.get(&scope).cloned();
                async move {
                    let (_, entry) = self
                        .fetch_observation(client, &account, &scope, etag.clone())
                        .await?;
                    Ok((scope, entry))
                }
            })
            .buffer_unordered(REFRESH_CONCURRENCY);
        tokio::pin!(fetches);
        let mut batch = Vec::with_capacity(PUBLICATION_BATCH);
        let mut completed = 0_usize;
        let mut failures = 0_usize;
        let mut last_error = None;
        while let Some(result) = fetches.next().await {
            match result {
                Ok((scope, mut entry)) => {
                    entry.content = contents
                        .entry(entry.content.digest)
                        .or_insert_with(|| entry.content.clone())
                        .clone();
                    batch.push((scope, entry));
                    completed += 1;
                    if completed == 1 || completed.is_multiple_of(PUBLICATION_BATCH) {
                        self.merge_observations(revision, std::mem::take(&mut batch))?;
                    }
                }
                Err(error) => {
                    failures += 1;
                    last_error = Some(error);
                }
            }
        }
        if !batch.is_empty() {
            self.merge_observations(revision, batch)?;
        }
        if let Some(error) = &last_error {
            tracing::warn!(successes=completed,failures,%error,"OpenAI account catalog refresh incomplete");
        }
        if pending
            .keys()
            .any(|k| self.cache.read().is_ok_and(|c| c.pending.contains_key(k)))
            && let Some(error) = last_error.clone()
        {
            self.refresh_notification.notify_one();
            return Err(error);
        }
        self.cached()?
            .ok_or(last_error.unwrap_or(CodexCredentialCatalogError::NoEligibleCredential))
    }
}
