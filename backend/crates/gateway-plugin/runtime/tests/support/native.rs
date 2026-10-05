//! 插件集成测试使用的原生 Provider 与执行事件替身

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use gateway_admin::{
    model::{
        observability::{CalculatedBillingBreakdown, DashboardWireProfile, ProviderBillingInput},
        provider_credentials::{
            AuthorizationStarted, CompleteAuthorization, PendingAuthorizationMutation,
            PrepareCredentialImport, PrepareCredentialRefresh, PrepareCredentialRotation,
            PreparedAuthorizationCommit, PreparedCredentialImport, PreparedCredentialRotation,
            ProviderExport, ProviderExportCredentialInput, ProviderModels, ProviderQuota,
            ProviderQuotaRequest,
        },
    },
    ports::provider::{
        ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind, ProviderAdminRegistry,
    },
};
use gateway_core::{
    account::ProviderAccountId,
    engine::{
        AttemptContext,
        provider::{
            Provider, ProviderCallMetadata, ProviderRegistry, ProviderRequest, ProviderStream,
        },
    },
    error::{ProviderError, ProviderErrorKind},
    event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta},
    operation::{Operation, OperationKind},
    routing::{
        ModelCapabilities, ProviderCatalogGeneration, ProviderKind, ProviderModelCapabilities,
        UpstreamModelId,
    },
    upstream::{UpstreamSendState, UpstreamTransport},
};
use serde_json::json;

pub const MODEL: &str = "native-fixture-model";

struct NativeProvider(&'static str);

#[async_trait]
impl Provider for NativeProvider {
    fn name(&self) -> &'static str {
        self.0
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        if self.0 != "openai" {
            return Ok(Vec::new());
        }
        Ok(vec![ProviderModelCapabilities::new(
            UpstreamModelId::new(MODEL).unwrap(),
            ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None),
        )])
    }

    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let account = context.required_account().cloned().ok_or_else(|| {
            ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
        })?;
        let candidate = request.candidate();
        let model = candidate.upstream_model().cloned().ok_or_else(|| {
            ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
        })?;
        let adapter = context.upstream_adapter(candidate.provider(), &model)?;
        let metadata = ProviderCallMetadata::new(
            candidate.provider().clone(),
            model,
            account,
            UpstreamTransport::new(
                adapter
                    .as_ref()
                    .map_or("http_sse", |adapter| adapter.transport()),
            )
            .unwrap(),
        );
        if let Some(adapter) = adapter {
            let account = Arc::new(FixtureConnection(metadata.provider_account_id().clone()));
            let events = adapter.execute(
                gateway_core::engine::upstream_adapter::UpstreamAdapterInvocation {
                    operation: request.operation().clone(),
                    headers: vec![],
                    context,
                    metadata: metadata.clone(),
                    account,
                },
            );
            return Ok(ProviderStream::new(metadata, events, ()));
        }
        let response = ResponseMeta::new("fixture-response", MODEL);
        let event = |name: &str, fact| {
            ProviderEvent::canonical_with_wire(
                vec![fact],
                ProtocolWireEvent::json(
                    "openai",
                    Some(name.to_owned()),
                    json!({"type":name,"response":{"id":"fixture-response","model":MODEL}}),
                )
                .unwrap(),
            )
        };
        Ok(ProviderStream::new(
            metadata,
            Box::pin(futures::stream::iter([
                Ok(event(
                    "response.created",
                    GatewayEvent::Started(response.clone()),
                )),
                Ok(event(
                    "response.completed",
                    GatewayEvent::Completed(response),
                )),
            ])),
            (),
        ))
    }
}

// 仅测试 Core / Runtime / Store 组合；真实 Provider 的凭据解释由各自合同测试验证
struct FixtureConnection(ProviderAccountId);

impl gateway_core::engine::upstream_adapter::UpstreamAccountConnection for FixtureConnection {
    fn account_id(&self) -> &ProviderAccountId {
        &self.0
    }
    fn credential_revision(&self) -> gateway_core::account::CredentialRevision {
        gateway_core::account::CredentialRevision::new(1).unwrap()
    }
    fn authentication_kind(&self) -> &str {
        "oauth"
    }
    fn outbound_proxy(&self) -> Option<&gateway_core::account::OutboundProxy> {
        None
    }
    fn authorization(
        &self,
    ) -> Result<Vec<gateway_core::engine::middleware::MiddlewareHeader>, ProviderError> {
        Ok(vec![
            gateway_core::engine::middleware::MiddlewareHeader::new(
                "authorization",
                bytes::Bytes::from_static(b"Bearer fixture-native-token"),
            ),
        ])
    }
    fn calculate_cost(
        &self,
        _: Option<&str>,
        _: &gateway_core::metering::Usage,
    ) -> Option<gateway_core::metering::CalculatedCost> {
        gateway_core::metering::CalculatedCost::from_usd_ticks(123).ok()
    }
    fn record_failure(
        &self,
        error: ProviderError,
    ) -> futures::future::BoxFuture<'_, ProviderError> {
        Box::pin(async move { error })
    }
}

pub fn provider_registry() -> ProviderRegistry {
    ProviderRegistry::new([
        Arc::new(NativeProvider("openai")) as Arc<dyn Provider>,
        Arc::new(NativeProvider("xai")) as Arc<dyn Provider>,
    ])
    .unwrap()
}

struct NativeAdmin(ProviderKind);

impl NativeAdmin {
    fn unsupported() -> ProviderAdminError {
        ProviderAdminError::new(ProviderAdminErrorKind::Unsupported)
    }
}

#[async_trait]
impl ProviderAdmin for NativeAdmin {
    fn provider_kind(&self) -> &ProviderKind {
        &self.0
    }

    async fn account_unavailable(&self, _: &ProviderAccountId) {}

    async fn connection_test_operation(
        &self,
        _: &UpstreamModelId,
        _: &str,
    ) -> Result<Operation, ProviderAdminError> {
        Err(Self::unsupported())
    }

    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile> {
        None
    }

    fn calculated_billing(
        &self,
        _: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError> {
        Ok(None)
    }

    async fn prepare_import(
        &self,
        _: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        Err(Self::unsupported())
    }

    async fn start_authorization(
        &self,
        _: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        Err(Self::unsupported())
    }

    async fn complete_authorization(
        &self,
        _: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        Err(Self::unsupported())
    }

    async fn prepare_rotation(
        &self,
        _: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        Err(Self::unsupported())
    }

    async fn prepare_refresh(
        &self,
        _: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        Err(Self::unsupported())
    }

    async fn quota(&self, _: ProviderQuotaRequest) -> Result<ProviderQuota, ProviderAdminError> {
        Err(Self::unsupported())
    }

    async fn models(
        &self,
        _: &ProviderAccountId,
        _: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        Err(Self::unsupported())
    }

    async fn export_credentials(
        &self,
        _: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError> {
        Err(Self::unsupported())
    }
}

pub fn admin_registry() -> ProviderAdminRegistry {
    ProviderAdminRegistry::new(["openai", "xai"].map(|name| {
        Arc::new(NativeAdmin(ProviderKind::new(name).unwrap())) as Arc<dyn ProviderAdmin>
    }))
    .unwrap()
}
