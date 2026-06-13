use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::RwLock, task::JoinHandle, time::Instant};
use tracing::{debug, error, info, warn};

use crate::{
    codex::accounts::{model::AccountStatus, service::AccountService},
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
pub struct RefreshScheduler {
    account_service: Arc<AccountService>,
    config: AppConfig,
    timers: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    in_flight: Arc<RwLock<HashMap<String, Instant>>>,
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
    "invalid_token",
    "access_denied",
    "refresh_token_expired",
];

impl RefreshScheduler {
    pub fn new(account_service: Arc<AccountService>, config: AppConfig) -> Self {
        Self {
            account_service,
            config,
            timers: Arc::new(RwLock::new(HashMap::new())),
            in_flight: Arc::new(RwLock::new(HashMap::new())),
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

        let delay = Duration::from_secs(refresh_at - now);
        info!(
            account_id = %account_id,
            delay_secs = delay.as_secs(),
            "已调度 token 刷新"
        );

        // 启动定时器 - 直接刷新，不再调用 do_refresh
        let account_id_clone = account_id.clone();
        let account_service = self.account_service.clone();
        let in_flight = self.in_flight.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;

            // 标记为正在处理
            {
                let mut in_flight_lock = in_flight.write().await;
                if in_flight_lock.contains_key(&account_id_clone) {
                    return; // 已经在刷新中
                }
                in_flight_lock.insert(account_id_clone.clone(), Instant::now());
            }

            // 执行刷新
            let _ = account_service.refresh_account(&account_id_clone).await;

            // 清理标记
            in_flight.write().await.remove(&account_id_clone);
        });

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

        let delay = Duration::from_secs(refresh_at - now);
        info!(
            account_id = %account_id,
            delay_secs = delay.as_secs(),
            "已调度下一次 token 刷新"
        );

        // 启动定时器
        let account_id_clone = account_id.clone();
        let account_service = self.account_service.clone();
        let in_flight = self.in_flight.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;

            {
                let mut in_flight_lock = in_flight.write().await;
                if in_flight_lock.contains_key(&account_id_clone) {
                    return;
                }
                in_flight_lock.insert(account_id_clone.clone(), Instant::now());
            }

            let _ = account_service.refresh_account(&account_id_clone).await;
            in_flight.write().await.remove(&account_id_clone);
        });

        self.timers.write().await.insert(account_id, handle);
        Ok(())
    }

    /// 调度恢复尝试（用于临时失败）
    async fn schedule_recovery(&self, account_id: String, delay: Duration) {
        let account_id_clone = account_id.clone();
        let account_service = self.account_service.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = account_service.refresh_account(&account_id_clone).await;
        });

        self.timers.write().await.insert(account_id, handle);
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
            match self.account_service.refresh_account(account_id).await {
                Ok(_result) => {
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
                Err(e) => {
                    let error_msg = format!("{}", e).to_lowercase();

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
                            // refresh_account 已经标记了状态，这里不需要再次标记
                            return Err(SchedulerError::AccountNotFound(format!(
                                "Permanent failure: {}",
                                e
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
                            error = %e,
                            "token 刷新尝试失败，准备重试"
                        );
                        tokio::time::sleep(delay).await;
                    } else {
                        error!(
                            account_id = %account_id,
                            attempts = MAX_ATTEMPTS,
                            error = %e,
                            "token 刷新重试次数已耗尽"
                        );
                        // 调度恢复尝试（不在循环内调用 await，避免递归）
                        let account_id_recovery = account_id.to_string();
                        let account_service = self.account_service.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(RECOVERY_DELAY_MS)).await;
                            let _ = account_service.refresh_account(&account_id_recovery).await;
                        });
                        return Err(SchedulerError::AccountNotFound(format!(
                            "Max attempts reached: {}",
                            e
                        )));
                    }
                }
            }
        }

        Ok(())
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
