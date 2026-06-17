use rand::RngExt;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::{RwLock, Semaphore},
    task::JoinHandle,
    time::Instant,
};
use tracing::{debug, error, info, warn};

use crate::{
    codex::accounts::{
        model::AccountStatus,
        service::{AccountProbeOutcome, AccountService},
    },
    config::AppConfig,
    runtime::tasks::types::{SchedulerError, SchedulerHandle},
};

pub type SchedulerResult<T> = Result<T, SchedulerError>;

/// OAuth 刷新调度器 - 在 JWT 过期前自动刷新访问令牌
///
/// 功能：
/// - 在 exp - margin 时刻调度刷新
/// - 指数退避（5次尝试：5s → 15s → 45s → 135s → 300s）
/// - 永久失败检测（invalid_grant / invalid_token）
/// - 临时失败的恢复调度（10分钟）
/// - 崩溃恢复：refreshing → 立即重试，expired + refreshToken → 延迟重试
#[derive(Clone)]
pub struct RefreshScheduler {
    account_service: Arc<AccountService>,
    config: AppConfig,
    timers: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    in_flight: Arc<RwLock<HashMap<String, Instant>>>,
    refresh_permits: Arc<Semaphore>,
    destroyed: Arc<RwLock<bool>>,
}

const MAX_ATTEMPTS: u32 = 5;
const BASE_DELAY_MS: u64 = 5_000;
const RECOVERY_DELAY_MS: u64 = 10 * 60 * 1000; // 10分钟
const PERMANENT_THRESHOLD: u32 = 2;

// 表示账户被封禁/停用的上游错误
const BAN_ERRORS: &[&str] = &["account has been deactivated", "refresh_token_reused"];

// 表示刷新令牌无效但不一定是封禁的错误
const EXPIRED_ERRORS: &[&str] = &[
    "invalid_grant",
    "invalidgrant",
    "invalid_token",
    "access_denied",
    "refresh_token_expired",
];

impl RefreshScheduler {
    pub fn new(account_service: Arc<AccountService>, config: AppConfig) -> Self {
        let refresh_permits = Arc::new(Semaphore::new(refresh_concurrency_limit(
            config.auth.refresh_concurrency,
        )));
        Self {
            account_service,
            config,
            timers: Arc::new(RwLock::new(HashMap::new())),
            in_flight: Arc::new(RwLock::new(HashMap::new())),
            refresh_permits,
            destroyed: Arc::new(RwLock::new(false)),
        }
    }

    /// 检查账户是否正在刷新（用于健康检查避免竞争）
    pub async fn is_refreshing(&self, account_id: &str) -> bool {
        self.in_flight.read().await.contains_key(account_id)
    }

    /// 为所有账户调度刷新
    pub async fn schedule_all(&self) -> SchedulerResult<()> {
        if !self.config.auth.refresh_enabled {
            info!("自动刷新已关闭（refresh_enabled = false）");
            return Ok(());
        }

        let accounts = self
            .account_service
            .list_all_for_refresh()
            .await
            .map_err(|_| SchedulerError::AccountNotFound("failed to list accounts".to_string()))?;

        let mut expired_index = 0;
        for account in accounts {
            // 跳过没有刷新令牌的账户
            if account.refresh_token.is_none() {
                continue;
            }

            // 跳过永久禁用/封禁的账户
            if matches!(
                account.status,
                AccountStatus::Disabled | AccountStatus::Banned
            ) {
                continue;
            }

            match account.status {
                AccountStatus::Refreshing => {
                    // 崩溃恢复：进程终止时正在刷新
                    info!(account_id = %account.id, "正在从 refreshing 状态恢复");
                    self.do_refresh(account.id.clone()).await;
                }
                AccountStatus::Expired => {
                    // 恢复尝试 - 每个账户间隔2秒以避免突发
                    let delay = Duration::from_millis(30_000 + expired_index * 2_000);
                    expired_index += 1;
                    info!(
                        account_id = %account.id,
                        delay_secs = delay.as_secs(),
                        "账户已过期，已调度恢复尝试"
                    );
                    self.schedule_recovery(account.id.clone(), delay).await;
                }
                _ => {
                    // active / quota_exhausted - 在令牌过期时调度刷新
                    use secrecy::ExposeSecret;
                    let token_str = account.access_token.expose_secret().to_string();
                    self.schedule_one(account.id.clone(), token_str).await?;
                }
            }
        }

        Ok(())
    }

    /// 触发账户的立即刷新（用于401触发的刷新）
    pub async fn trigger_refresh_now(&self, account_id: String) -> SchedulerResult<()> {
        // 检查账户是否正在刷新
        if self.is_refreshing(&account_id).await {
            debug!(account_id = %account_id, "账户正在刷新，跳过本次触发");
            return Ok(());
        }

        // 取消现有定时器
        self.clear_one(&account_id).await;

        // 执行刷新
        self.do_refresh(account_id).await;
        Ok(())
    }

    /// 为单个账户调度刷新
    async fn schedule_one(&self, account_id: String, access_token: String) -> SchedulerResult<()> {
        // 清除现有定时器
        self.clear_one(&account_id).await;

        // 解析 JWT 获取过期时间
        let exp = match self.parse_jwt_exp(&access_token) {
            Some(exp) => exp,
            None => {
                warn!(account_id = %account_id, "解析 JWT exp 失败，跳过刷新调度");
                return Ok(());
            }
        };

        let margin = self.config.auth.refresh_margin_seconds;
        let now = chrono::Utc::now().timestamp() as u64;
        let refresh_at = exp.saturating_sub(margin);

        if refresh_at <= now {
            // 已经过了刷新时间 - 立即尝试刷新
            debug!(account_id = %account_id, "token 已超过刷新时间，立即刷新");
            self.do_refresh(account_id).await;
            return Ok(());
        }

        let delay = jitter_delay(Duration::from_secs(refresh_at - now));
        info!(
            account_id = %account_id,
            delay_secs = delay.as_secs(),
            "已调度 token 刷新"
        );

        let handle = self.spawn_delayed_refresh(account_id.clone(), delay);

        self.timers.write().await.insert(account_id, handle);
        Ok(())
    }

    /// 调度下次刷新（在刷新成功后调用，避免递归）
    async fn schedule_next_refresh(
        &self,
        account_id: String,
        access_token: String,
    ) -> SchedulerResult<()> {
        // 清除现有定时器
        self.clear_one(&account_id).await;

        // 解析 JWT 获取过期时间
        let exp = match self.parse_jwt_exp(&access_token) {
            Some(exp) => exp,
            None => {
                warn!(account_id = %account_id, "解析 JWT exp 失败，跳过刷新调度");
                return Ok(());
            }
        };

        let margin = self.config.auth.refresh_margin_seconds;
        let now = chrono::Utc::now().timestamp() as u64;
        let refresh_at = exp.saturating_sub(margin);

        if refresh_at <= now {
            // 已经过了刷新时间
            debug!(account_id = %account_id, "刷新后 token 仍已超过下次刷新时间");
            return Ok(());
        }

        let delay = jitter_delay(Duration::from_secs(refresh_at - now));
        info!(
            account_id = %account_id,
            delay_secs = delay.as_secs(),
            "已调度下一次 token 刷新"
        );

        let handle = self.spawn_delayed_refresh(account_id.clone(), delay);

        self.timers.write().await.insert(account_id, handle);
        Ok(())
    }

    /// 调度恢复尝试（用于临时失败）
    async fn schedule_recovery(&self, account_id: String, delay: Duration) {
        let handle = self.spawn_delayed_refresh(account_id.clone(), jitter_delay(delay));

        self.timers.write().await.insert(account_id, handle);
    }

    fn spawn_delayed_refresh(&self, account_id: String, delay: Duration) -> JoinHandle<()> {
        let scheduler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // 定时器触发后先移除自身，避免刷新成功后调度下一次时 abort 当前任务。
            scheduler.timers.write().await.remove(&account_id);
            scheduler.do_refresh(account_id).await;
        })
    }

    /// 取消单个账户的定时器
    async fn clear_one(&self, account_id: &str) {
        if let Some(handle) = self.timers.write().await.remove(account_id) {
            handle.abort();
        }
    }

    /// 执行刷新（带并发控制）
    async fn do_refresh(&self, account_id: String) {
        // 检查是否已销毁
        if *self.destroyed.read().await {
            return;
        }

        // 检查是否已在刷新中
        {
            let mut in_flight = self.in_flight.write().await;
            if in_flight.contains_key(&account_id) {
                debug!(account_id = %account_id, "刷新任务已在执行，跳过");
                return;
            }
            in_flight.insert(account_id.clone(), Instant::now());
        }

        let _permit = match self.refresh_permits.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(error) => {
                self.in_flight.write().await.remove(&account_id);
                error!(
                    account_id = %account_id,
                    error = %error,
                    "refresh 并发控制已关闭"
                );
                return;
            }
        };

        // 执行刷新逻辑
        let result = self.do_refresh_inner(&account_id).await;

        // 清理 in_flight 标记
        self.in_flight.write().await.remove(&account_id);

        if let Err(e) = result {
            error!(account_id = %account_id, error = %e, "token 刷新失败");
        }
    }

    /// 内部刷新逻辑（带重试和错误处理）
    #[tracing::instrument(skip(self), fields(account_id = %account_id))]
    async fn do_refresh_inner(&self, account_id: &str) -> SchedulerResult<()> {
        info!(account_id = %account_id, "开始刷新 token");

        let mut permanent_hits = 0;

        for attempt in 1..=MAX_ATTEMPTS {
            let error = match self
                .account_service
                .probe_scheduled_account_refresh(account_id)
                .await
            {
                Ok(result) if matches!(result.outcome, AccountProbeOutcome::Alive) => {
                    info!(account_id = %account_id, "token 刷新成功");
                    // 获取新令牌并重新调度下次刷新
                    if let Ok(accounts) = self
                        .account_service
                        .export(vec![account_id.to_string()])
                        .await
                    {
                        if let Some(account) = accounts.first() {
                            // 从 SecretString 中提取字符串
                            use secrecy::ExposeSecret;
                            let token_str = account.access_token.expose_secret().to_string();
                            // 不调用 schedule_one 避免递归，直接创建定时器
                            let _ = self
                                .schedule_next_refresh(account_id.to_string(), token_str)
                                .await;
                        }
                    }
                    return Ok(());
                }
                Ok(result) if matches!(result.outcome, AccountProbeOutcome::Skipped) => {
                    info!(
                        account_id = %account_id,
                        reason = result.error.as_deref().unwrap_or("skipped"),
                        "token 刷新已跳过"
                    );
                    return Ok(());
                }
                Ok(result) => result
                    .error
                    .unwrap_or_else(|| "token refresh failed".to_string()),
                Err(error) => error.to_string(),
            };

            let error_msg = error.to_lowercase();

            // 检查是否是永久错误
            let is_ban = BAN_ERRORS.iter().any(|&err| error_msg.contains(err));
            let is_expired = EXPIRED_ERRORS.iter().any(|&err| error_msg.contains(err));
            let is_permanent = is_ban || is_expired;

            if is_permanent {
                permanent_hits += 1;
                if permanent_hits >= PERMANENT_THRESHOLD {
                    error!(
                        account_id = %account_id,
                        hits = permanent_hits,
                        "检测到永久刷新失败"
                    );
                    let status = if is_ban { "banned" } else { "expired" };
                    if let Err(status_error) =
                        self.account_service.update_status(account_id, status).await
                    {
                        error!(
                            account_id = %account_id,
                            status,
                            error = ?status_error,
                            "写入永久刷新失败状态失败"
                        );
                    }
                    return Err(SchedulerError::AccountNotFound(format!(
                        "Permanent failure: {}",
                        error
                    )));
                }
                warn!(
                    account_id = %account_id,
                    hits = permanent_hits,
                    threshold = PERMANENT_THRESHOLD,
                    "检测到永久错误，继续重试确认"
                );
            }

            if attempt < MAX_ATTEMPTS {
                // 指数退避：5s, 15s, 45s, 135s, 300s（有上限）
                let backoff = BASE_DELAY_MS.saturating_mul(3_u64.pow(attempt - 1));
                let backoff = backoff.min(300_000);
                let delay = Duration::from_millis(backoff);

                warn!(
                    account_id = %account_id,
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    delay_secs = delay.as_secs(),
                    error = %error,
                    "token 刷新尝试失败，准备重试"
                );
                tokio::time::sleep(delay).await;
            } else {
                error!(
                    account_id = %account_id,
                    attempts = MAX_ATTEMPTS,
                    error = %error,
                    "token 刷新重试次数已耗尽"
                );
                self.restore_account_for_recovery(account_id).await;
                self.schedule_recovery(
                    account_id.to_string(),
                    Duration::from_millis(RECOVERY_DELAY_MS),
                )
                .await;
                return Err(SchedulerError::AccountNotFound(format!(
                    "Max attempts reached: {}",
                    error
                )));
            }
        }

        Ok(())
    }

    async fn restore_account_for_recovery(&self, account_id: &str) {
        match self
            .account_service
            .update_status(account_id, "active")
            .await
        {
            Ok(Some(_)) => {
                info!(
                    account_id = %account_id,
                    "临时 refresh 失败后已恢复账号状态"
                );
            }
            Ok(None) => {
                warn!(
                    account_id = %account_id,
                    "临时 refresh 失败后恢复账号状态失败：账号不存在"
                );
            }
            Err(error) => {
                warn!(
                    account_id = %account_id,
                    error = ?error,
                    "临时 refresh 失败后恢复账号状态失败"
                );
            }
        }
    }

    /// 解析 JWT 获取过期时间
    fn parse_jwt_exp(&self, token: &str) -> Option<u64> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return None;
        }

        let payload = parts[1];
        use base64::Engine;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()?;
        let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
        json.get("exp")?.as_u64()
    }

    /// 启动调度器并返回句柄
    pub async fn start(self) -> SchedulerHandle {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel(1);
        let scheduler = Arc::new(self);

        // 初始调度所有账户
        let _ = scheduler.schedule_all().await;

        // 启动后台任务监听关闭信号
        let scheduler_clone = scheduler.clone();
        tokio::spawn(async move {
            shutdown_rx.recv().await;
            *scheduler_clone.destroyed.write().await = true;

            // 取消所有定时器
            let mut timers = scheduler_clone.timers.write().await;
            for handle in timers.values() {
                handle.abort();
            }
            timers.clear();

            info!("token 刷新调度器已关闭");
        });

        SchedulerHandle::new(shutdown_tx)
    }

    /// 销毁调度器
    pub async fn destroy(&self) {
        *self.destroyed.write().await = true;

        // 取消所有定时器
        let mut timers = self.timers.write().await;
        for handle in timers.values() {
            handle.abort();
        }
        timers.clear();

        // 清理 in_flight
        self.in_flight.write().await.clear();
    }
}

fn refresh_concurrency_limit(configured: u32) -> usize {
    configured.max(1) as usize
}

fn jitter_delay(delay: Duration) -> Duration {
    let mut rng = rand::rng();
    jitter_delay_with_factor(delay, rng.random_range(0.8..=1.2))
}

fn jitter_delay_with_factor(delay: Duration, factor: f64) -> Duration {
    let factor = factor.clamp(0.8, 1.2);
    let millis = (delay.as_millis() as f64 * factor)
        .round()
        .min(u64::MAX as f64);
    Duration::from_millis(millis as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use secrecy::SecretString;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::Mutex;

    use crate::{
        codex::{
            accounts::{
                pool::AccountPool,
                repository::{AccountRepository, AccountUsageRepository, NewAccount},
                service::{AccountService, AccountServiceDependencies},
            },
            gateway::{
                fingerprint::model::Fingerprint,
                oauth::{RefreshFailure, TokenPair, TokenRefresher},
                transport::websocket::CodexWebSocketPool,
            },
        },
        config::{
            AdminConfig, ApiConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
            QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
            UsageStatsConfig,
        },
        platform::{crypto::SecretBox, storage::db::connect_sqlite},
    };

    #[test]
    fn refresh_concurrency_limit_should_never_be_zero() {
        assert_eq!(refresh_concurrency_limit(0), 1);
    }

    #[test]
    fn jitter_delay_with_factor_should_keep_delay_inside_original_range() {
        assert_eq!(
            jitter_delay_with_factor(Duration::from_secs(100), 0.8),
            Duration::from_secs(80)
        );
        assert_eq!(
            jitter_delay_with_factor(Duration::from_secs(100), 1.2),
            Duration::from_secs(120)
        );
    }

    #[tokio::test]
    async fn do_refresh_inner_should_mark_expired_only_after_second_permanent_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("refresh-scheduler.sqlite");
        let url = format!("sqlite://{}", db.display());
        let pool = connect_sqlite(&url).await.unwrap();
        let repo = AccountRepository::new(pool.clone(), SecretBox::new([41u8; 32]));
        repo.insert(NewAccount {
            id: "acct_scheduler".to_string(),
            email: None,
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-old".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-old".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        })
        .await
        .unwrap();
        let observed_statuses = Arc::new(Mutex::new(Vec::new()));
        let refresher = AlwaysInvalidGrantRefresher {
            repo: repo.clone(),
            account_id: "acct_scheduler".to_string(),
            calls: Arc::new(AtomicUsize::new(0)),
            observed_statuses: observed_statuses.clone(),
        };
        let config = test_config(url);
        let account_service = Arc::new(AccountService::new(
            Arc::new(config.clone()),
            AccountServiceDependencies {
                repository: Some(repo.clone()),
                usage_repository: Some(AccountUsageRepository::new(pool)),
                cookie_repository: None,
                token_refresher: Some(Arc::new(refresher.clone())),
                account_pool: Arc::new(Mutex::new(AccountPool::default())),
                websocket_pool: Arc::new(CodexWebSocketPool::with_default_max_age()),
                fingerprint: Fingerprint::default_for_tests(),
            },
        ));
        let scheduler = RefreshScheduler::new(account_service, config);

        let result = scheduler.do_refresh_inner("acct_scheduler").await;

        assert!(result.is_err());
        assert_eq!(refresher.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            observed_statuses.lock().await.as_slice(),
            [AccountStatus::Refreshing, AccountStatus::Refreshing]
        );
        let stored = repo.get("acct_scheduler").await.unwrap().unwrap();
        assert_eq!(stored.status, AccountStatus::Expired);
    }

    #[tokio::test]
    async fn do_refresh_inner_should_persist_refreshing_during_refresh_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("refresh-scheduler-refreshing.sqlite");
        let url = format!("sqlite://{}", db.display());
        let pool = connect_sqlite(&url).await.unwrap();
        let repo = AccountRepository::new(pool.clone(), SecretBox::new([43u8; 32]));
        repo.insert(NewAccount {
            id: "acct_refreshing".to_string(),
            email: None,
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-old".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-old".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        })
        .await
        .unwrap();
        let observed_statuses = Arc::new(Mutex::new(Vec::new()));
        let refresher = StatusObservingSuccessRefresher {
            repo: repo.clone(),
            account_id: "acct_refreshing".to_string(),
            observed_statuses: observed_statuses.clone(),
        };
        let config = test_config(url);
        let account_service = Arc::new(AccountService::new(
            Arc::new(config.clone()),
            AccountServiceDependencies {
                repository: Some(repo.clone()),
                usage_repository: Some(AccountUsageRepository::new(pool)),
                cookie_repository: None,
                token_refresher: Some(Arc::new(refresher)),
                account_pool: Arc::new(Mutex::new(AccountPool::default())),
                websocket_pool: Arc::new(CodexWebSocketPool::with_default_max_age()),
                fingerprint: Fingerprint::default_for_tests(),
            },
        ));
        let scheduler = RefreshScheduler::new(account_service, config);

        let result = scheduler.do_refresh_inner("acct_refreshing").await;

        assert!(result.is_ok());
        assert_eq!(
            observed_statuses.lock().await.as_slice(),
            [AccountStatus::Refreshing]
        );
        let stored = repo.get("acct_refreshing").await.unwrap().unwrap();
        assert_eq!(stored.status, AccountStatus::Active);
    }

    #[tokio::test]
    async fn do_refresh_inner_should_restore_refreshing_account_after_transient_failures() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("refresh-scheduler-recovery.sqlite");
        let url = format!("sqlite://{}", db.display());
        let pool = connect_sqlite(&url).await.unwrap();
        let repo = AccountRepository::new(pool.clone(), SecretBox::new([42u8; 32]));
        repo.insert(NewAccount {
            id: "acct_recovery".to_string(),
            email: None,
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-old".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-old".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Refreshing,
        })
        .await
        .unwrap();
        let refresher = AlwaysTransportFailingRefresher::default();
        let config = test_config(url);
        let account_service = Arc::new(AccountService::new(
            Arc::new(config.clone()),
            AccountServiceDependencies {
                repository: Some(repo.clone()),
                usage_repository: Some(AccountUsageRepository::new(pool)),
                cookie_repository: None,
                token_refresher: Some(Arc::new(refresher.clone())),
                account_pool: Arc::new(Mutex::new(AccountPool::default())),
                websocket_pool: Arc::new(CodexWebSocketPool::with_default_max_age()),
                fingerprint: Fingerprint::default_for_tests(),
            },
        ));
        let scheduler = RefreshScheduler::new(account_service, config);
        tokio::time::pause();
        let running_scheduler = scheduler.clone();
        let refresh =
            tokio::spawn(async move { running_scheduler.do_refresh_inner("acct_recovery").await });

        for expected_calls in 1..=MAX_ATTEMPTS as usize {
            wait_for_refresh_calls(&refresher.calls, expected_calls).await;
        }

        let result = refresh.await.unwrap();

        assert!(result.is_err());
        assert_eq!(
            refresher.calls.load(Ordering::SeqCst),
            MAX_ATTEMPTS as usize
        );
        let stored = repo.get("acct_recovery").await.unwrap().unwrap();
        assert_eq!(stored.status, AccountStatus::Active);
        assert!(scheduler.timers.read().await.contains_key("acct_recovery"));
        scheduler.destroy().await;
    }

    async fn wait_for_refresh_calls(calls: &AtomicUsize, expected: usize) {
        for _ in 0..160 {
            if calls.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::task::yield_now().await;
            if calls.load(Ordering::SeqCst) >= expected {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
            if calls.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::time::advance(Duration::from_secs(5)).await;
        }
        let actual = calls.load(Ordering::SeqCst);
        assert!(
            actual >= expected,
            "expected at least {expected} refresh calls, got {actual}"
        );
    }

    #[derive(Clone)]
    struct AlwaysInvalidGrantRefresher {
        repo: AccountRepository,
        account_id: String,
        calls: Arc<AtomicUsize>,
        observed_statuses: Arc<Mutex<Vec<AccountStatus>>>,
    }

    #[async_trait]
    impl TokenRefresher for AlwaysInvalidGrantRefresher {
        async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let status = self
                .repo
                .get(&self.account_id)
                .await
                .unwrap()
                .unwrap()
                .status;
            self.observed_statuses.lock().await.push(status);
            Err(RefreshFailure::InvalidGrant)
        }
    }

    #[derive(Clone)]
    struct StatusObservingSuccessRefresher {
        repo: AccountRepository,
        account_id: String,
        observed_statuses: Arc<Mutex<Vec<AccountStatus>>>,
    }

    #[async_trait]
    impl TokenRefresher for StatusObservingSuccessRefresher {
        async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
            let status = self
                .repo
                .get(&self.account_id)
                .await
                .unwrap()
                .unwrap()
                .status;
            self.observed_statuses.lock().await.push(status);
            Ok(TokenPair {
                access_token: "access-new".to_string(),
                refresh_token: None,
            })
        }
    }

    #[derive(Clone, Default)]
    struct AlwaysTransportFailingRefresher {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl TokenRefresher for AlwaysTransportFailingRefresher {
        async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(RefreshFailure::Transport)
        }
    }

    fn test_config(database_url: String) -> AppConfig {
        AppConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
            },
            api: ApiConfig {
                base_url: "https://chatgpt.com/backend-api".to_string(),
            },
            model: ModelConfig {
                default_model: "gpt-5.5".to_string(),
                default_reasoning_effort: None,
                service_tier: None,
                aliases: Default::default(),
            },
            auth: AuthConfig {
                refresh_margin_seconds: 300,
                refresh_enabled: true,
                refresh_concurrency: 2,
                max_concurrent_per_account: 3,
                request_interval_ms: 50,
                rotation_strategy: "least_used".to_string(),
                tier_priority: Vec::new(),
                oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
                oauth_auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
                oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
            },
            quota: QuotaConfig {
                refresh_interval_minutes: 5,
                warning_thresholds: QuotaWarningThresholds {
                    primary: vec![80, 90],
                    secondary: vec![80, 90],
                },
                skip_exhausted: true,
            },
            usage_stats: UsageStatsConfig {
                history_retention_days: None,
            },
            database: DatabaseConfig { url: database_url },
            security: SecurityConfig {
                master_key_file: "data/master.key".to_string(),
                api_key_pepper_file: "data/api-key-pepper.key".to_string(),
            },
            tls: TlsConfig {
                force_http11: false,
            },
            ws_pool: Default::default(),
            admin: AdminConfig {
                session_ttl_minutes: 1440,
                default_username: "admin".to_string(),
                default_password: "admin".to_string(),
                session_cleanup_interval_secs: 3600,
            },
            logging: LoggingConfig {
                directory: "logs".to_string(),
                retention_days: 14,
                enabled: false,
                capacity: 2_000,
                capture_body: false,
            },
        }
    }
}
