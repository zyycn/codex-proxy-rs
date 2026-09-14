//! 强制会话 Key 范围的自助观测查询；会话生命周期归属 AuthService。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use crate::{
    AuthService, ObservabilityService,
    model::{
        AdminError, AdminErrorKind, PageSize,
        auth::SessionSubject,
        client_usage::{ClientOverview, ClientUsageKey},
        observability::{
            DiagnosticDimension, DiagnosticsResult, OpsErrorPage, OpsErrorQuery, TimeRange,
            UsageFilter, UsageInsights, UsagePage, UsageQuery, UsageSummary,
        },
    },
    ports::{
        provider::ProviderAdminRegistry,
        store::{ClientUsageStore, ObservabilityStore},
    },
};

use super::{map_store_error, observability::health_timeline_at};

#[async_trait]
pub trait ClientUsageService: Send + Sync {
    async fn overview(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
    ) -> Result<Option<ClientOverview>, AdminError>;

    async fn records(
        &self,
        session_id: Option<&str>,
        query: UsageQuery,
    ) -> Result<Option<UsagePage>, AdminError>;

    async fn summary(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
    ) -> Result<Option<UsageSummary>, AdminError>;

    async fn insights(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
    ) -> Result<Option<UsageInsights>, AdminError>;

    async fn diagnostics(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
        dimension: DiagnosticDimension,
    ) -> Result<Option<DiagnosticsResult>, AdminError>;

    async fn errors(
        &self,
        session_id: Option<&str>,
        query: OpsErrorQuery,
    ) -> Result<Option<OpsErrorPage>, AdminError>;
}

pub(crate) struct DefaultClientUsageService {
    auth: Arc<dyn AuthService>,
    store: Arc<dyn ClientUsageStore>,
    observability_store: Arc<dyn ObservabilityStore>,
    observability: Arc<dyn ObservabilityService>,
    providers: ProviderAdminRegistry,
}

impl DefaultClientUsageService {
    pub(crate) fn new(
        auth: Arc<dyn AuthService>,
        store: Arc<dyn ClientUsageStore>,
        observability_store: Arc<dyn ObservabilityStore>,
        observability: Arc<dyn ObservabilityService>,
        providers: ProviderAdminRegistry,
    ) -> Self {
        Self {
            auth,
            store,
            observability_store,
            observability,
            providers,
        }
    }

    async fn resolve_key(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<ClientUsageKey>, AdminError> {
        let Some(session) = self.auth.session(session_id).await? else {
            return Ok(None);
        };
        let SessionSubject::Key { client_key_id } = session.subject else {
            return Err(AdminError::new(
                AdminErrorKind::Forbidden,
                "当前身份无权访问密钥用量接口",
            ));
        };
        self.store
            .load_client_usage_key(&client_key_id)
            .await
            .map_err(|error| map_store_error(error, "client usage key"))
    }

    fn usage_filter(key: &ClientUsageKey) -> UsageFilter {
        UsageFilter {
            client_api_key_ref: Some(key.id.as_str().to_owned()),
            ..UsageFilter::default()
        }
    }

    fn scope_usage_query(key: &ClientUsageKey, mut query: UsageQuery) -> UsageQuery {
        query.filter.client_api_key_ref = Some(key.id.as_str().to_owned());
        query.filter.provider_kind = None;
        query.filter.provider_account_ref = None;
        query
    }

    fn scope_error_query(key: &ClientUsageKey, mut query: OpsErrorQuery) -> OpsErrorQuery {
        query.filter.client_api_key_ref = Some(key.id.as_str().to_owned());
        query.filter.provider_kind = None;
        query.filter.provider_account_ref = None;
        query
    }
}

#[async_trait]
impl ClientUsageService for DefaultClientUsageService {
    async fn overview(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
    ) -> Result<Option<ClientOverview>, AdminError> {
        let Some(key) = self.resolve_key(session_id).await? else {
            return Ok(None);
        };
        let filter = Self::usage_filter(&key);
        let trend_filter = filter.clone();
        let records_filter = filter.clone();
        let totals = async {
            self.store
                .load_client_usage_totals(&key.id)
                .await
                .map_err(|error| map_store_error(error, "client usage totals"))
        };
        let trend = async {
            self.observability_store
                .usage_trend(range, trend_filter)
                .await
                .map_err(|error| map_store_error(error, "client usage trend"))
        };
        let records = self.observability.usage_records(UsageQuery {
            range,
            filter: records_filter,
            current_page: 1,
            page_size: PageSize::new(10).expect("client overview page size is valid"),
        });
        let (summary, totals, trend, records) = futures::try_join!(
            self.observability.usage_summary(range, filter),
            totals,
            trend,
            records,
        )?;
        let wire_profiles = self.providers.dashboard_wire_profiles();
        let health_timeline = health_timeline_at(&trend, Utc::now());
        Ok(Some(ClientOverview {
            as_of: key.observed_at,
            key,
            range,
            overview: summary.overview,
            totals,
            trend,
            health_timeline,
            wire_profiles,
            usage_records: records.items,
        }))
    }

    async fn records(
        &self,
        session_id: Option<&str>,
        query: UsageQuery,
    ) -> Result<Option<UsagePage>, AdminError> {
        let Some(key) = self.resolve_key(session_id).await? else {
            return Ok(None);
        };
        self.observability
            .usage_records(Self::scope_usage_query(&key, query))
            .await
            .map(Some)
    }

    async fn summary(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
    ) -> Result<Option<UsageSummary>, AdminError> {
        let Some(key) = self.resolve_key(session_id).await? else {
            return Ok(None);
        };
        self.observability
            .usage_summary(range, Self::usage_filter(&key))
            .await
            .map(Some)
    }

    async fn insights(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
    ) -> Result<Option<UsageInsights>, AdminError> {
        let Some(key) = self.resolve_key(session_id).await? else {
            return Ok(None);
        };
        self.observability
            .usage_insights(range, Self::usage_filter(&key))
            .await
            .map(Some)
    }

    async fn diagnostics(
        &self,
        session_id: Option<&str>,
        range: TimeRange,
        dimension: DiagnosticDimension,
    ) -> Result<Option<DiagnosticsResult>, AdminError> {
        let Some(key) = self.resolve_key(session_id).await? else {
            return Ok(None);
        };
        self.observability
            .diagnostics(range, Self::usage_filter(&key), dimension)
            .await
            .map(Some)
    }

    async fn errors(
        &self,
        session_id: Option<&str>,
        query: OpsErrorQuery,
    ) -> Result<Option<OpsErrorPage>, AdminError> {
        let Some(key) = self.resolve_key(session_id).await? else {
            return Ok(None);
        };
        self.observability
            .ops_errors(Self::scope_error_query(&key, query))
            .await
            .map(Some)
    }
}
