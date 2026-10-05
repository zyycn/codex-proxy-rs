//! Key 自助查询从统一会话或只读 Key 校验取得范围，复用现有观测和额度账本

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    AuthService, SystemService,
    model::{
        AdminError, AdminErrorKind,
        auth::SessionSubject,
        client_keys::ClientKeySecret,
        key_usage::{
            KeyUsageOverview, KeyUsageQuery, KeyUsageRecordKind, KeyUsageRecords,
            KeyUsageRecordsQuery,
        },
        observability::{OpsErrorFilter, OpsErrorQuery, TimeRange, UsageFilter, UsageQuery},
        system::SystemVersion,
    },
    ports::store::{ClientKeyStore, ObservabilityStore},
};
use gateway_core::{
    engine::{
        budget::ClientBudgetStatus,
        execution::{ClientAuthenticationError, ClientKeyVerifier},
    },
    policy::ClientApiKeyId,
};

use super::{map_store_error, observability::health_timeline_at};

#[async_trait]
pub trait KeyUsageService: Send + Sync {
    /// 验证 Key 并只读查询当前额度，不记录 Key 使用或执行推理准入
    async fn budget(&self, plaintext: &str) -> Result<Option<ClientBudgetStatus>, AdminError>;

    /// 使用 Core 已认证的宿主身份查询额度，不接收插件自报的 Key ID
    async fn budget_for_client(
        &self,
        id: &ClientApiKeyId,
    ) -> Result<Option<ClientBudgetStatus>, AdminError>;

    async fn version(&self, session_id: Option<&str>) -> Result<Option<SystemVersion>, AdminError>;

    async fn config(&self, session_id: Option<&str>)
    -> Result<Option<ClientKeySecret>, AdminError>;

    async fn overview(
        &self,
        session_id: Option<&str>,
        query: KeyUsageQuery,
    ) -> Result<Option<KeyUsageOverview>, AdminError>;

    async fn records(
        &self,
        session_id: Option<&str>,
        query: KeyUsageRecordsQuery,
    ) -> Result<Option<KeyUsageRecords>, AdminError>;
}

pub(crate) struct DefaultKeyUsageService {
    timezone: gateway_core::time::DeploymentTimeZone,
    auth: Arc<dyn AuthService>,
    verifier: Arc<dyn ClientKeyVerifier>,
    keys: Arc<dyn ClientKeyStore>,
    observations: Arc<dyn ObservabilityStore>,
    system: Arc<dyn SystemService>,
}

impl DefaultKeyUsageService {
    pub(crate) fn new(
        auth: Arc<dyn AuthService>,
        verifier: Arc<dyn ClientKeyVerifier>,
        keys: Arc<dyn ClientKeyStore>,
        observations: Arc<dyn ObservabilityStore>,
        system: Arc<dyn SystemService>,
        timezone: gateway_core::time::DeploymentTimeZone,
    ) -> Self {
        Self {
            timezone,
            auth,
            verifier,
            keys,
            observations,
            system,
        }
    }

    async fn key_id(&self, session_id: Option<&str>) -> Result<Option<ClientApiKeyId>, AdminError> {
        match self
            .auth
            .session(session_id)
            .await?
            .map(|session| session.subject)
        {
            Some(SessionSubject::Key { client_key_id }) => Ok(Some(client_key_id)),
            Some(SessionSubject::Admin { .. }) => Err(AdminError::new(
                AdminErrorKind::Forbidden,
                "当前身份无权访问密钥用量接口",
            )),
            None => Ok(None),
        }
    }
}

fn usage_filter(id: &ClientApiKeyId, model: Option<String>) -> UsageFilter {
    UsageFilter {
        client_api_key_ref: Some(id.as_str().to_owned()),
        model,
        ..UsageFilter::default()
    }
}

#[async_trait]
impl KeyUsageService for DefaultKeyUsageService {
    async fn budget(&self, plaintext: &str) -> Result<Option<ClientBudgetStatus>, AdminError> {
        let id = match self.verifier.verify_client_key(plaintext) {
            Ok(id) => id,
            Err(ClientAuthenticationError::InvalidKey) => return Ok(None),
            Err(
                ClientAuthenticationError::SnapshotUnavailable
                | ClientAuthenticationError::ProviderUnavailable,
            ) => {
                return Err(AdminError::new(
                    AdminErrorKind::Unavailable,
                    "密钥验证暂时不可用",
                ));
            }
        };
        self.budget_for_client(&id).await
    }

    async fn budget_for_client(
        &self,
        id: &ClientApiKeyId,
    ) -> Result<Option<ClientBudgetStatus>, AdminError> {
        self.keys
            .get_client_key(id)
            .await
            .map(|key| key.filter(|key| key.enabled).map(|key| key.budget))
            .map_err(|error| map_store_error(error, "key usage budget"))
    }

    async fn version(&self, session_id: Option<&str>) -> Result<Option<SystemVersion>, AdminError> {
        if self.key_id(session_id).await?.is_none() {
            return Ok(None);
        }
        self.system.version().await.map(Some)
    }

    async fn config(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<ClientKeySecret>, AdminError> {
        let Some(id) = self.key_id(session_id).await? else {
            return Ok(None);
        };
        // 明文只按服务端会话绑定的 Key 读取，禁用或删除后不再提供配置
        self.keys
            .reveal_client_key(&id)
            .await
            .map(|secret| secret.filter(|secret| secret.record.enabled))
            .map_err(|error| map_store_error(error, "key usage config"))
    }

    async fn overview(
        &self,
        session_id: Option<&str>,
        query: KeyUsageQuery,
    ) -> Result<Option<KeyUsageOverview>, AdminError> {
        let Some(id) = self.key_id(session_id).await? else {
            return Ok(None);
        };
        let Some(key) = self
            .keys
            .get_client_key(&id)
            .await
            .map_err(|error| map_store_error(error, "key usage profile"))?
            .filter(|key| key.enabled)
        else {
            return Ok(None);
        };
        let filter = usage_filter(&id, query.model);
        let now = query.range.end;
        // 健康条始终展示部署时区今日，不随历史范围或模型筛选改变
        let today = TimeRange {
            start: self
                .timezone
                .day_start(now)
                .ok_or_else(|| AdminError::internal("时间超出支持范围"))?,
            end: now,
        };
        let (overview, trend, health_points) = futures::try_join!(
            self.observations.usage_summary(query.range, filter.clone()),
            self.observations.usage_trend(query.range, filter),
            self.observations
                .usage_trend(today, usage_filter(&id, None)),
        )
        .map_err(|error| map_store_error(error, "key usage overview"))?;
        Ok(Some(KeyUsageOverview {
            key,
            overview,
            trend,
            health_timeline: health_timeline_at(&health_points, now, self.timezone)?,
        }))
    }

    async fn records(
        &self,
        session_id: Option<&str>,
        query: KeyUsageRecordsQuery,
    ) -> Result<Option<KeyUsageRecords>, AdminError> {
        let Some(id) = self.key_id(session_id).await? else {
            return Ok(None);
        };
        let result = match query.kind {
            KeyUsageRecordKind::Success => KeyUsageRecords::Success(
                self.observations
                    .list_usage_records(UsageQuery {
                        range: query.usage.range,
                        filter: usage_filter(&id, query.usage.model),
                        current_page: query.current_page,
                        page_size: query.page_size,
                    })
                    .await
                    .map_err(|error| map_store_error(error, "key usage records"))?,
            ),
            KeyUsageRecordKind::Error => KeyUsageRecords::Error(
                self.observations
                    .list_ops_errors(OpsErrorQuery {
                        range: query.usage.range,
                        filter: OpsErrorFilter {
                            client_api_key_ref: Some(id.as_str().to_owned()),
                            model: query.usage.model,
                            ..OpsErrorFilter::default()
                        },
                        current_page: query.current_page,
                        page_size: query.page_size,
                    })
                    .await
                    .map_err(|error| map_store_error(error, "key usage errors"))?,
            ),
        };
        Ok(Some(result))
    }
}
