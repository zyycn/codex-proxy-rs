//! Key 自助查询只从统一会话取得范围，复用现有观测和额度账本。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use crate::{
    AuthService,
    model::{
        AdminError, AdminErrorKind,
        auth::SessionSubject,
        key_usage::{
            KeyUsageOverview, KeyUsageQuery, KeyUsageRecordKind, KeyUsageRecords,
            KeyUsageRecordsQuery,
        },
        observability::{
            OpsErrorFilter, OpsErrorQuery, TimeRange, UsageFilter, UsageQuery, china_day_start,
        },
    },
    ports::store::{ClientKeyStore, ObservabilityStore},
};
use gateway_core::policy::ClientApiKeyId;

use super::{map_store_error, observability::health_timeline_at};

#[async_trait]
pub trait KeyUsageService: Send + Sync {
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
    auth: Arc<dyn AuthService>,
    keys: Arc<dyn ClientKeyStore>,
    observations: Arc<dyn ObservabilityStore>,
}

impl DefaultKeyUsageService {
    pub(crate) fn new(
        auth: Arc<dyn AuthService>,
        keys: Arc<dyn ClientKeyStore>,
        observations: Arc<dyn ObservabilityStore>,
    ) -> Self {
        Self {
            auth,
            keys,
            observations,
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
        let now = Utc::now();
        // 健康条始终展示北京时间今日，不随历史范围或模型筛选改变。
        let today = TimeRange {
            start: china_day_start(now),
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
            health_timeline: health_timeline_at(&health_points, now),
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
