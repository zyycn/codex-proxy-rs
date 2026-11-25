//! 管理端账号操作与客户端画像的适配入口

use crate::credential::catalog::CatalogService;
use crate::{
    credential::{AuthorizationService, CredentialService, oauth::error},
    profile::ReleaseService,
};
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
    ports::provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind as Kind},
};
use gateway_core::{
    account::{OpaqueProviderData, ProviderAccountId},
    operation::Operation,
    routing::{ProviderKind, UpstreamModelId},
};
use std::sync::Arc;

pub(crate) struct AntigravityAdmin {
    pub(crate) kind: ProviderKind,
    pub(crate) credentials: Arc<CredentialService>,
    pub(crate) authorization: AuthorizationService,
    pub(crate) catalog: Arc<CatalogService>,
    pub(crate) release: Arc<ReleaseService>,
    pub(crate) quota: Arc<crate::credential::quota::QuotaService>,
}

#[async_trait]
impl ProviderAdmin for AntigravityAdmin {
    fn is_direct_account(&self, authentication_kind: &str) -> bool {
        authentication_kind == "oauth"
    }
    async fn peer_model_facts(
        &self,
        accounts: &[gateway_admin::model::accounts::AccountRecord],
    ) -> Result<
        Option<
            std::collections::BTreeMap<
                String,
                gateway_admin::ports::provider::ProviderPeerAccountModels,
            >,
        >,
        ProviderAdminError,
    > {
        self.quota.peer_model_facts(accounts).await.map(Some)
    }
    fn account_capabilities(
        &self,
        _: &ProviderAccountId,
        authentication_kind: &str,
    ) -> gateway_admin::model::accounts::ProviderAccountCapabilities {
        let oauth = authentication_kind == "oauth";
        gateway_admin::model::accounts::ProviderAccountCapabilities {
            credential_refresh: oauth,
            reauthorize: oauth,
            credential_export: oauth,
            quota: oauth,
            quota_refresh: oauth,
            ..Default::default()
        }
    }
    fn plan_type_display(&self, plan: &str) -> String {
        match plan {
            "g1-pro-tier" => "Google AI Pro",
            "free-tier" => "Antigravity",
            _ => plan,
        }
        .to_owned()
    }
    fn supported_pricing_bands(&self) -> &'static [&'static str] {
        crate::billing::SUPPORTED_PRICING_BANDS
    }

    fn pricing_catalog(&self) -> gateway_admin::model::pricing::ProviderPricingCatalog {
        crate::billing::defaults().clone()
    }
    fn pricing_source(&self) -> gateway_admin::ports::pricing::PricingCatalogSource {
        gateway_admin::ports::pricing::PricingCatalogSource::References(
            crate::billing::source_models(),
        )
    }
    fn evaluate_usage_pricing(
        &self,
        input: &gateway_admin::ports::provider::ProviderUsagePricingInput,
        pricing: Option<&gateway_core::metering::ModelPriceOverride>,
    ) -> Result<Option<gateway_core::metering::CalculatedCostBreakdown>, ProviderAdminError> {
        Ok(crate::billing::calculate(
            &input.upstream_model_id,
            &input.usage,
            pricing,
        ))
    }
    fn provider_kind(&self) -> &ProviderKind {
        &self.kind
    }
    async fn account_unavailable(&self, _: &ProviderAccountId) {
        self.catalog.invalidate();
    }
    async fn account_facts_changed(&self, ids: &[ProviderAccountId]) {
        self.catalog.invalidate();
        for id in ids {
            let _ = self.catalog.account_models(id, false).await;
        }
    }
    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile> {
        Some(crate::profile::dashboard(
            &crate::profile::Selection::default(),
            &self.release,
        ))
    }
    fn configured_wire_profile(
        &self,
        configuration: &OpaqueProviderData,
    ) -> Option<DashboardWireProfile> {
        Some(crate::profile::dashboard(
            &crate::profile::Selection::parse(configuration).ok()?,
            &self.release,
        ))
    }
    fn default_client_profile(&self) -> Option<OpaqueProviderData> {
        Some(crate::profile::Selection::default().configuration())
    }
    fn client_profile_options(&self) -> Result<OpaqueProviderData, ProviderAdminError> {
        Ok(crate::profile::options())
    }
    fn preview_client_profile(
        &self,
        configuration: &OpaqueProviderData,
    ) -> Result<OpaqueProviderData, ProviderAdminError> {
        crate::profile::preview(configuration, &self.release)
    }
    fn calculated_billing(
        &self,
        _: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError> {
        // 此历史接口没有请求当时的价格快照，不能用当前人工价重算历史费用
        Ok(None)
    }

    async fn start_authorization(
        &self,
        pending: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        self.authorization.start(pending).await
    }
    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        self.authorization.complete(command).await
    }
    async fn prepare_refresh(
        &self,
        command: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        let id = ProviderAccountId::new(command.account.id).map_err(|_| error(Kind::Invalid))?;
        let current = self.credentials.current(&id).await?;
        if command.account.provider_kind != self.kind
            || current.account.revision().get() != command.account.credential_revision.get()
        {
            return Err(error(Kind::Conflict));
        }
        self.credentials.refresh_current(&current).await
    }

    async fn prepare_import(
        &self,
        command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        self.credentials.prepare_import(command).await
    }
    async fn prepare_rotation(
        &self,
        command: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        self.credentials.prepare_rotation(command).await
    }
    async fn models(
        &self,
        id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        self.catalog.account_models(id, refresh).await
    }
    async fn connection_test_operation(
        &self,
        _: &UpstreamModelId,
        input: &str,
    ) -> Result<Operation, ProviderAdminError> {
        let body = serde_json::to_vec(
            &serde_json::json!({"contents":[{"role":"user","parts":[{"text":input}]}]}),
        )
        .map_err(|_| error(Kind::Internal))?;
        let payload = gateway_core::operation::ProtocolPayload::from_json_bytes(
            gateway_protocol::genai::GENAI_PROTOCOL,
            body.into(),
        )
        .map_err(|_| error(Kind::Invalid))?;
        Ok(Operation::Generate(
            gateway_core::operation::GenerateRequest::from_protocol_payload(payload),
        ))
    }
    async fn quota(
        &self,
        request: ProviderQuotaRequest,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        if request.refresh {
            self.quota.refresh(&request.account_id).await?;
        }
        self.quota.read(&request.account_id).await
    }
    async fn export_credentials(
        &self,
        credentials: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError> {
        self.credentials.export_credentials(credentials)
    }
}
