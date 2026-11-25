//! 管理端口的唯一实现与 Provider 服务协调

use super::credentials::{
    account_from_record, account_matches_record, prepared_create, prepared_rotation,
    rotation_secret, validate_account_record,
};
use super::error::{
    credential_admin_error_code, log_import_failure, map_catalog_error, map_client_profile_error,
    map_credential_admin_error, map_oauth_error, map_profile_avatar_error,
    map_profile_statistics_error, map_quota_error, map_reset_credits_error, map_store_error,
    oauth_error_code, provider_admin_error, provider_admin_error_code,
};
use super::profile::{
    dashboard_desktop_release, project_profile_avatar, project_profile_statistics,
    project_reset_credit, project_reset_credits,
};
use super::quota;
use super::quota::{empty_quota, project_quota, quota_role};
use crate::billing::{OpenAiBillingUsage, openai_billing_breakdown};
use crate::credential::{
    CodexCredentialAdmin, CodexCredentialAdminService, CodexCredentialCatalogService,
    CodexCredentialCodec, CodexCredentialProfileService, CodexCredentialQuotaService,
    CodexOAuthAdmin, CompleteCodexOAuthAuthorization, CompletedCodexOAuthCredential,
    ExportManagedCodexCredential, StartCodexOAuthAuthorization, oauth_owner_ref,
    parse_access_token_expiration,
};
use crate::profile::{CodexDesktopReleaseStatus, CodexWireProfileState};
use crate::transport::CodexWebSocketPool;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use gateway_admin::model::accounts::AccountRecord;
use gateway_admin::model::observability::{
    CalculatedBillingBreakdown, CurrencyCost, DashboardDesktopRelease, DashboardWireAttribute,
    DashboardWireProfile, DashboardWireTarget, DecimalAmount, DesktopReleaseStatus,
    ProviderBillingInput,
};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwnerBinding, AuthorizationStarted,
    CompleteAuthorization, ConsumeProviderResetCredit, PrepareCredentialImport,
    PrepareCredentialRefresh, PrepareCredentialRotation, PreparedAuthorizationCommit,
    PreparedAuthorizationCredential, PreparedCredentialImport, PreparedCredentialRotation,
    ProviderDocument, ProviderExport, ProviderExportCredentialInput, ProviderModel,
    ProviderModelCatalogDocument, ProviderModels, ProviderProfileAvatar, ProviderProfileStatistics,
    ProviderQuota, ProviderQuotaReadRequest, ProviderQuotaRequest, ProviderQuotaWindow,
    ProviderResetCreditResult, ProviderResetCredits, ProviderSubscription,
    QuotaLocalUsageAttribution,
};
use gateway_admin::model::quota_forecast::QuotaForecastObservation;
use gateway_admin::ports::provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::account::{
    CredentialRevision, LoadedCredential, NewProviderAccount, OpaqueProviderData,
    PlaintextCredential, ProviderAccount, ProviderAccountId, ProviderAccountStore,
};
use gateway_core::metering::Money;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload, RawJsonPayload};
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use gateway_protocol::openai::OPENAI_PROTOCOL;
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// OpenAI 对终态 Admin port 的唯一实现
pub(crate) struct OpenAiAdminProvider {
    provider_kind: ProviderKind,
    profile: CodexWireProfileState,
    accounts: Arc<dyn ProviderAccountStore>,
    credentials: Arc<CodexCredentialAdminService>,
    oauth: Arc<dyn CodexOAuthAdmin>,
    profile_statistics: Arc<CodexCredentialProfileService>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
    websocket_pool: Arc<CodexWebSocketPool>,
    desktop_release: CodexDesktopReleaseStatus,
}

pub(crate) struct OpenAiAdminServices {
    pub(crate) credentials: Arc<CodexCredentialAdminService>,
    pub(crate) oauth: Arc<dyn CodexOAuthAdmin>,
    pub(crate) profile_statistics: Arc<CodexCredentialProfileService>,
    pub(crate) quota: Arc<CodexCredentialQuotaService>,
    pub(crate) catalog: Arc<CodexCredentialCatalogService>,
}

impl OpenAiAdminProvider {
    #[must_use]
    pub(crate) fn new(
        provider_kind: ProviderKind,
        profile: CodexWireProfileState,
        accounts: Arc<dyn ProviderAccountStore>,
        services: OpenAiAdminServices,
        websocket_pool: Arc<CodexWebSocketPool>,
        desktop_release: CodexDesktopReleaseStatus,
    ) -> Self {
        Self {
            provider_kind,
            profile,
            accounts,
            credentials: services.credentials,
            oauth: services.oauth,
            profile_statistics: services.profile_statistics,
            quota: services.quota,
            catalog: services.catalog,
            websocket_pool,
            desktop_release,
        }
    }

    async fn account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderAccount, ProviderAdminError> {
        self.accounts
            .get_account(account_id)
            .await
            .map_err(map_store_error)?
            .filter(|account| account.provider() == &self.provider_kind)
            .ok_or_else(|| provider_admin_error(ProviderAdminErrorKind::NotFound))
    }

    async fn preserve_existing_installation_id(
        &self,
        mut incoming: NewProviderAccount,
    ) -> Result<NewProviderAccount, ProviderAdminError> {
        let Some(existing) = self
            .accounts
            .get_account(incoming.account.id())
            .await
            .map_err(map_store_error)?
        else {
            return Ok(incoming);
        };
        if existing.provider() != incoming.account.provider()
            || existing.upstream_user_id() != incoming.account.upstream_user_id()
            || existing.upstream_account_id() != incoming.account.upstream_account_id()
        {
            return Ok(incoming);
        }
        let current = self
            .accounts
            .load_current_credential(existing.id())
            .await
            .map_err(map_store_error)?;
        incoming.credential = CodexCredentialCodec::preserve_installation_id(
            &incoming.credential,
            &current.credential,
        )
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
        Ok(incoming)
    }
}

#[async_trait]
impl ProviderAdmin for OpenAiAdminProvider {
    fn is_direct_account(&self, authentication_kind: &str) -> bool {
        crate::credential::is_direct_account_kind(authentication_kind)
    }

    fn peer_model_eligible(&self, account: &AccountRecord, model: &str) -> Option<bool> {
        account_from_record(account).ok().map(|account| {
            crate::credential::CodexSelectionModel::Requested(model).allows_account(&account)
        })
    }

    fn compile_privacy_policy(
        &self,
        policy: &gateway_core::settings::privacy::CodexPrivacyPolicy,
    ) -> Result<
        Arc<dyn gateway_core::settings::privacy::CompiledPrivacyPolicy>,
        gateway_core::settings::privacy::PrivacyError,
    > {
        crate::transport::privacy::compile(policy)
    }

    fn preview_privacy_policy(
        &self,
        request: gateway_core::settings::privacy::PrivacyPreviewRequest,
    ) -> Result<
        gateway_core::settings::privacy::PrivacyPreviewResult,
        gateway_core::settings::privacy::PrivacyError,
    > {
        crate::transport::privacy::preview(request)
    }

    fn account_capabilities(
        &self,
        _account_id: &ProviderAccountId,
        authentication_kind: &str,
    ) -> gateway_admin::model::accounts::ProviderAccountCapabilities {
        let oauth = authentication_kind == crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH;
        gateway_admin::model::accounts::ProviderAccountCapabilities {
            credential_refresh: oauth,
            reauthorize: oauth,
            credential_export: oauth
                || authentication_kind == crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY,
            model_catalog_download: oauth,
            quota_settings_supported: oauth,
            quota_credits: oauth,
            quota: oauth,
            quota_refresh: oauth,
            profile: oauth,
            subscription: oauth,
            avatar: oauth,
            reset_credits: oauth,
            consume_reset_credit: oauth,
        }
    }

    fn supported_pricing_bands(&self) -> &'static [&'static str] {
        crate::billing::SUPPORTED_PRICING_BANDS
    }

    fn pricing_catalog(&self) -> gateway_admin::model::pricing::ProviderPricingCatalog {
        crate::billing::pricing_catalog()
    }

    fn client_profile_options(
        &self,
    ) -> Result<gateway_core::account::OpaqueProviderData, ProviderAdminError> {
        self.profile
            .selection_options()
            .map_err(map_client_profile_error)
    }

    fn preview_client_profile(
        &self,
        configuration: &gateway_core::account::OpaqueProviderData,
    ) -> Result<gateway_core::account::OpaqueProviderData, ProviderAdminError> {
        if configuration
            .expose_to_provider()
            .contains_key("installationSeed")
            || gateway_core::settings::request_profiles::inherited_fields(configuration).is_some()
        {
            return Err(provider_admin_error(ProviderAdminErrorKind::Invalid)
                .with_public_message("安装身份种子只能在 API 密钥中设置"));
        }
        self.profile
            .preview_selection(configuration)
            .map_err(map_client_profile_error)
    }

    fn preview_key_client_profile(
        &self,
        key: &gateway_core::account::OpaqueProviderData,
        global: Option<&gateway_core::account::OpaqueProviderData>,
    ) -> Result<gateway_core::account::OpaqueProviderData, ProviderAdminError> {
        crate::profile::identity::inherited_identity_fields(key)
            .map_err(map_client_profile_error)?;
        let profile = gateway_core::settings::request_profiles::resolve_key_profile(key, global);
        self.profile
            .preview_selection(&profile)
            .map_err(map_client_profile_error)
    }

    fn merge_batch_key_profile(
        &self,
        current: Option<&gateway_core::account::OpaqueProviderData>,
        update: Option<&Option<gateway_core::account::OpaqueProviderData>>,
        installation_seed: Option<uuid::Uuid>,
    ) -> Result<Option<gateway_core::account::OpaqueProviderData>, ProviderAdminError> {
        crate::profile::identity::merge_batch_key_profile(current, update, installation_seed)
            .map_err(map_client_profile_error)
    }

    fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    fn plan_type_display(&self, plan_type: &str) -> String {
        // Pro 三档沿用管理端短名，其余已知套餐使用 Codex /status 的名称
        match plan_type.to_ascii_lowercase().as_str() {
            "free" => "Free",
            "go" => "Go",
            "plus" => "Plus",
            "pro" => "Pro",
            "prolite" => "ProLite",
            "promax" => "ProMax",
            "team" | "self_serve_business_usage_based" => "Business",
            "self_serve_business_prolite" => "Business Premium",
            "business" | "ent26" | "enterprise" | "hc" | "enterprise_cbp_usage_based" => {
                "Enterprise"
            }
            "enterprise_cbp_automation" => "Enterprise (Automation)",
            "edu" | "education" => "Edu",
            "edu_plus" => "Edu Plus",
            "edu_pro" => "Edu Pro",
            "sci" => "Academic Researchers",
            "sci_plus" => "Academic Researchers Plus",
            _ => plan_type,
        }
        .to_owned()
    }

    async fn account_unavailable(&self, account_id: &ProviderAccountId) {
        self.websocket_pool.evict_account(account_id.as_str()).await;
    }

    async fn account_facts_changed(&self, account_ids: &[ProviderAccountId]) {
        if account_ids.is_empty() {
            return;
        }
        self.quota.invalidate_scheduling(account_ids);
        if let Err(error) = self.catalog.invalidate_accounts(account_ids).await {
            tracing::warn!(
                account_count = account_ids.len(),
                error = %error,
                "OpenAI model catalog invalidation failed after account commit"
            );
        }
    }

    async fn connection_test_operation(
        &self,
        upstream_model: &UpstreamModelId,
        input_text: &str,
    ) -> Result<Operation, ProviderAdminError> {
        build_connection_test_operation(upstream_model, input_text)
    }

    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile> {
        let profile = self.profile.snapshot();
        let release = self.desktop_release.snapshot();
        let user_agent = profile.user_agent();
        let client_identity = format!("{}; {}", profile.originator, profile.desktop_version);
        let release = dashboard_desktop_release(&profile, release);
        Some(DashboardWireProfile {
            provider: self.provider_kind.as_str().to_owned(),
            product: profile.originator,
            version: profile.codex_version,
            build: None,
            target: DashboardWireTarget {
                os_type: profile.os_type,
                os_version: profile.os_version,
                arch: profile.arch,
                terminal: profile.terminal,
            },
            user_agent,
            attributes: vec![DashboardWireAttribute {
                key: "client_identifier".to_owned(),
                value: client_identity,
            }],
            verified_at: Some(profile.verified_at),
            release: Some(release),
        })
    }

    fn configured_wire_profile(
        &self,
        configuration: &OpaqueProviderData,
    ) -> Option<DashboardWireProfile> {
        use crate::profile::identity::RequestProfileSelection;
        use crate::profile::selection::{ClientKind, ClientPlatform, VersionMode};
        let selection = RequestProfileSelection::parse(configuration).ok()?;
        let profile = selection.resolve(&self.profile).ok()?;
        let custom = selection.version_mode() == VersionMode::Fixed;
        let release = selection.preset().and_then(|selection| {
            let (checked_at, error) = self.profile.client_release_status(
                selection.client,
                selection.platform,
                selection.architecture(),
            );
            if custom {
                None
            } else if selection.client == ClientKind::Desktop
                && selection.platform == ClientPlatform::Macos
            {
                Some(dashboard_desktop_release(
                    &profile,
                    self.desktop_release.snapshot(),
                ))
            } else {
                Some(DashboardDesktopRelease {
                    status: if error.is_some() {
                        DesktopReleaseStatus::Failed
                    } else if checked_at.is_some() {
                        DesktopReleaseStatus::Current
                    } else {
                        DesktopReleaseStatus::Unchecked
                    },
                    checked_at,
                    latest_version: Some(profile.codex_version.clone()),
                    latest_build: None,
                    published_at: None,
                    minimum_system_version: None,
                    hardware_requirements: None,
                    download_url: None,
                    download_size: None,
                    signature_present: None,
                    error,
                })
            }
        });
        Some(DashboardWireProfile {
            provider: self.provider_kind.as_str().to_owned(),
            product: profile.originator.clone(),
            version: profile.codex_version.clone(),
            build: None,
            user_agent: profile.user_agent(),
            target: DashboardWireTarget {
                os_type: profile.os_type,
                os_version: profile.os_version,
                arch: profile.arch,
                terminal: profile.terminal,
            },
            attributes: vec![
                DashboardWireAttribute {
                    key: "client_identifier".to_owned(),
                    value: if profile.client_kind == ClientKind::Desktop {
                        format!("{}; {}", profile.originator, profile.desktop_version)
                    } else {
                        profile.originator
                    },
                },
                DashboardWireAttribute {
                    key: "version_policy".to_owned(),
                    value: if custom { "fixed" } else { "latest" }.to_owned(),
                },
            ],
            verified_at: (!custom && profile.verified_at != chrono::DateTime::UNIX_EPOCH)
                .then_some(profile.verified_at),
            release,
        })
    }

    fn calculated_billing(
        &self,
        input: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError> {
        let (Some(input_tokens), Some(output_tokens)) = (input.input_tokens, input.output_tokens)
        else {
            return Ok(None);
        };
        let Some(breakdown) = openai_billing_breakdown(
            &input.upstream_model_id,
            OpenAiBillingUsage::new(
                input_tokens,
                output_tokens,
                input.cached_input_tokens.unwrap_or_default(),
                input.cache_write_input_tokens.unwrap_or_default(),
            ),
            input.service_tier.as_deref(),
        ) else {
            return Ok(None);
        };
        let total_amount = currency_cost(breakdown.total_amount())?;
        if total_amount != input.total {
            return Ok(None);
        }
        Ok(Some(CalculatedBillingBreakdown {
            // 历史总额只能核对费用拆分，不能证明当时保存过长上下文标记
            long_context_billing_applied: false,
            image: None,
            custom_multiplier_bps: breakdown.custom_multiplier_bps(),
            input_amount: currency_cost(breakdown.input_amount())?,
            output_amount: currency_cost(breakdown.output_amount())?,
            cache_read_amount: currency_cost(breakdown.cache_read_amount())?,
            cache_write_amount: currency_cost(breakdown.cache_write_amount())?,
            standard_amount: currency_cost(breakdown.standard_amount())?,
            total_amount,
            input_price_per_million: currency_cost(breakdown.input_price_per_million())?,
            output_price_per_million: currency_cost(breakdown.output_price_per_million())?,
            cache_read_price_per_million: currency_cost(breakdown.cache_read_price_per_million())?,
            cache_write_price_per_million: currency_cost(
                breakdown.cache_write_price_per_million(),
            )?,
            service_tier: breakdown.service_tier().map(str::to_owned),
            multiplier_percent: breakdown.multiplier_percent(),
        }))
    }

    fn evaluate_usage_pricing(
        &self,
        input: &gateway_admin::ports::provider::ProviderUsagePricingInput,
        pricing: Option<&gateway_core::metering::ModelPriceOverride>,
    ) -> Result<Option<gateway_core::metering::CalculatedCostBreakdown>, ProviderAdminError> {
        let (Some(input_tokens), Some(output_tokens), Some(cached), Some(written)) = (
            input.usage.input_tokens,
            input.usage.output_tokens,
            input.usage.cached_input_tokens,
            input.usage.cache_write_input_tokens,
        ) else {
            return Ok(None);
        };
        if input.usage.image_input_tokens != Some(0) || input.usage.image_output_tokens != Some(0) {
            return Ok(None);
        }
        let usage = OpenAiBillingUsage::new(input_tokens, output_tokens, cached, written);
        Ok(crate::billing::openai_billing_breakdown_with_override(
            &input.upstream_model_id,
            usage,
            input.service_tier.as_deref(),
            pricing,
        ))
    }

    async fn prepare_import(
        &self,
        command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        let prepared = self
            .credentials
            .prepare_import_document_with_proxy(
                Value::Object(command.document.into_provider_data().into_inner()),
                command.default_outbound_proxy.as_ref(),
            )
            .await
            .inspect_err(|error| {
                log_import_failure("prepare_document", credential_admin_error_code(error));
            })
            .map_err(map_credential_admin_error)?;
        let observed_at = Utc::now();
        let mut credentials = Vec::with_capacity(prepared.accounts().len());
        for account in prepared.into_accounts() {
            let account = self
                .preserve_existing_installation_id(account)
                .await
                .inspect_err(|error| {
                    log_import_failure(
                        "preserve_installation_id",
                        provider_admin_error_code(error.kind()),
                    );
                })?;
            credentials.push(prepared_create(account, observed_at)?);
        }
        Ok(PreparedCredentialImport {
            provider_kind: self.provider_kind.clone(),
            credentials,
        })
    }

    async fn start_authorization(
        &self,
        pending: gateway_admin::model::provider_credentials::PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        if pending.provider_kind() != &self.provider_kind {
            return Err(provider_admin_error(ProviderAdminErrorKind::Invalid));
        }
        if let gateway_admin::model::provider_credentials::AuthorizationMutationTarget::Reauthorize { account_id } = pending.target()
            && self.account(account_id).await?.authentication_kind() != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH {
            return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
        }
        let started = self
            .oauth
            .start_authorization(StartCodexOAuthAuthorization { mutation: pending })
            .await
            .map_err(map_oauth_error)?;
        Ok(AuthorizationStarted {
            flow_id: started.flow_id,
            authorization_url: started.authorization_url,
            expires_at: started.expires_at,
        })
    }

    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        let request_id = command.context.request_id.clone();
        let binding = AuthorizationOwnerBinding::from_context(&command.context);
        let completed = self
            .oauth
            .complete_authorization(CompleteCodexOAuthAuthorization {
                owner_ref: oauth_owner_ref(binding.owner()),
                flow_id: command.flow_id,
                callback_url: SecretString::from(command.callback_url),
            })
            .await
            .inspect_err(|error| {
                tracing::warn!(
                    request_id = %request_id,
                    oauth_stage = "complete_authorization",
                    oauth_error = oauth_error_code(error),
                    "OpenAI OAuth authorization completion failed"
                );
            })
            .map_err(map_oauth_error)?;
        let (mutation, completed_credential, authorization_guard) = completed.into_parts();
        let credential = match (mutation.target(), completed_credential) {
            (
                AuthorizationMutationTarget::Create { .. },
                CompletedCodexOAuthCredential::Create(credential),
            ) => prepared_create(*credential, Utc::now())
                .map(|credential| PreparedAuthorizationCredential::Create(Box::new(credential))),
            (
                AuthorizationMutationTarget::Reauthorize { .. },
                CompletedCodexOAuthCredential::Reauthorize(credential),
            ) => {
                prepared_rotation(*credential, mutation.provider_kind().clone()).map(|credential| {
                    PreparedAuthorizationCredential::Reauthorize(Box::new(credential))
                })
            }
            _ => Err(provider_admin_error(ProviderAdminErrorKind::Internal)),
        };
        let credential = match credential {
            Ok(credential) => credential,
            Err(error) => {
                if let Err(settlement_error) = authorization_guard.abort().await {
                    tracing::warn!(
                        request_id = %request_id,
                        settlement_error = %settlement_error,
                        "OpenAI OAuth claim release failed after credential preparation"
                    );
                    return Err(provider_admin_error(ProviderAdminErrorKind::Unavailable));
                }
                return Err(error);
            }
        };
        Ok(PreparedAuthorizationCommit::new(mutation, credential)
            .with_authorization_guard(authorization_guard))
    }

    async fn prepare_rotation(
        &self,
        command: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        validate_account_record(&command.account, &self.provider_kind)?;
        let account_id = ProviderAccountId::new(command.account.id.clone())
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
        let current = self
            .accounts
            .load_current_credential(&account_id)
            .await
            .map_err(map_store_error)?;
        if !account_matches_record(&current.account, &command.account) {
            return Err(provider_admin_error(ProviderAdminErrorKind::Conflict)
                .with_public_message("凭据已更新，请刷新账号列表"));
        }
        if current.account.authentication_kind()
            == crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY
        {
            let prepared = CodexCredentialAdmin
                .prepare_api_key_rotation(
                    current,
                    Value::Object(command.provider_material.into_provider_data().into_inner()),
                )
                .map_err(map_credential_admin_error)?;
            return prepared_rotation(prepared, command.account.provider_kind);
        }
        if command
            .provider_material
            .expose_to_provider()
            .expose_to_provider()
            .contains_key("transport")
        {
            let prepared = CodexCredentialAdmin
                .prepare_transport_update(
                    current,
                    Value::Object(command.provider_material.into_provider_data().into_inner()),
                )
                .map_err(map_credential_admin_error)?;
            return prepared_rotation(prepared, command.account.provider_kind)
                .map(PreparedCredentialRotation::preserving_credential_state);
        }
        let mut secret = rotation_secret(command.provider_material)?;
        if secret.id_token.is_none() {
            let runtime = CodexCredentialCodec::decode(&current.credential)
                .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
            secret.id_token = runtime
                .authentication
                .oauth()
                .and_then(|oauth| oauth.id_token.clone());
        }
        let access_token_expires_at =
            parse_access_token_expiration(secret.access_token.expose_secret());
        let prepared = CodexCredentialAdmin
            .prepare_oauth_rotation(current, secret, None, access_token_expires_at, None)
            .map_err(map_credential_admin_error)?;
        prepared_rotation(prepared, command.account.provider_kind)
    }

    async fn prepare_refresh(
        &self,
        command: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        if command.account.authentication_kind != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
        }
        validate_account_record(&command.account, &self.provider_kind)?;
        let account_id = ProviderAccountId::new(command.account.id.clone())
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
        let current = self
            .accounts
            .load_current_credential(&account_id)
            .await
            .map_err(map_store_error)?;
        if !account_matches_record(&current.account, &command.account) {
            return Err(provider_admin_error(ProviderAdminErrorKind::Conflict)
                .with_public_message("凭据已更新，请刷新账号列表"));
        }
        let prepared = self
            .credentials
            .manual_refresh(current)
            .await
            .map_err(map_credential_admin_error)?;
        prepared_rotation(prepared, command.account.provider_kind)
    }

    async fn account_configuration(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderDocument>, ProviderAdminError> {
        self.account(account_id).await?;
        let current = self
            .accounts
            .load_current_credential(account_id)
            .await
            .map_err(map_store_error)?;
        let data = CodexCredentialCodec::decode_complete(&current.credential)
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
        let value = match data {
            crate::credential::CodexCredentialData::ApiKey(data) => {
                serde_json::to_value(data.configuration())
                    .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?
            }
            crate::credential::CodexCredentialData::OAuth(data) => {
                serde_json::json!({"transport": data.transport})
            }
        };
        let object = value
            .as_object()
            .cloned()
            .ok_or_else(|| provider_admin_error(ProviderAdminErrorKind::Internal))?;
        Ok(Some(ProviderDocument::new(
            gateway_core::account::OpaqueProviderData::new(object),
        )))
    }

    async fn quota(
        &self,
        request: ProviderQuotaRequest,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        let ProviderQuotaRequest {
            account_id,
            refresh,
            rolling_usage: _,
        } = request;
        let mut account = self.account(&account_id).await?;
        if account.authentication_kind() == crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY {
            if refresh {
                return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
            }
            return Ok(empty_quota());
        }
        let snapshot = if refresh {
            let snapshot = Some(
                self.quota
                    .refresh_account(&account_id)
                    .await
                    .map_err(map_quota_error)?,
            );
            account = self.account(&account_id).await?;
            snapshot
        } else {
            self.quota
                .read_snapshot_for(&account)
                .await
                .map_err(map_quota_error)?
        };
        Ok(project_quota(
            snapshot,
            account.quota().is_exhausted(),
            account.quota_settings(),
        ))
    }

    async fn read_quotas(
        &self,
        requests: &[ProviderQuotaReadRequest<'_>],
    ) -> Vec<Result<ProviderQuota, ProviderAdminError>> {
        self.read_quota_snapshots(requests).await
    }

    fn peer_authentication_kinds(&self) -> &[&str] {
        &[crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY]
    }

    fn peer_connection(
        &self,
        loaded: &gateway_core::account::LoadedCredential,
    ) -> Result<Option<gateway_admin::ports::peer::PeerConnection>, ProviderAdminError> {
        if loaded.account.provider() != &self.provider_kind
            || !self.supports_peer_connection(loaded.account.authentication_kind())
        {
            return Ok(None);
        }
        let credential = CodexCredentialCodec::decode(&loaded.credential)
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
        match credential.authentication {
            crate::credential::CodexRuntimeAuthentication::ApiKey(authentication) => {
                Ok(Some(gateway_admin::ports::peer::PeerConnection {
                    base_url: authentication.configuration.base_url,
                    bearer_token: authentication.secret,
                }))
            }
            crate::credential::CodexRuntimeAuthentication::OAuth(_) => Ok(None),
        }
    }

    fn quota_history_parser(
        &self,
    ) -> Option<gateway_admin::model::quota_history::QuotaHistoryParser> {
        Some(quota::history_reading)
    }

    fn quota_forecast_observation(
        &self,
        document: &ProviderDocument,
        window: &ProviderQuotaWindow,
    ) -> Option<QuotaForecastObservation> {
        if window.local_usage_attribution != QuotaLocalUsageAttribution::AccountWide {
            return None;
        }
        let (observed, plan_type) = quota::history_window(document, &window.key)?;
        if Some(observed.source()) != window.limit_id.as_deref()
            || Some(quota_role(observed.role())) != window.role
            || observed.window_seconds()? != window.window_seconds?
        {
            return None;
        }
        Some(QuotaForecastObservation {
            used_percent: observed.used_percent()?,
            reset_at: observed.reset_at()?,
            plan_type,
        })
    }

    async fn subscription(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<ProviderSubscription>, ProviderAdminError> {
        if self.account(account_id).await?.authentication_kind()
            != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
        }
        self.profile_statistics
            .subscription(account_id)
            .await
            .map(|subscription| {
                subscription.map(|subscription| ProviderSubscription {
                    starts_at: subscription.starts_at,
                    expires_at: subscription.expires_at,
                    will_renew: subscription.will_renew,
                    billing_period: subscription.billing_period,
                    billing_currency: subscription.billing_currency,
                    observed_at: subscription.observed_at,
                })
            })
            .map_err(map_profile_statistics_error)
    }

    async fn profile_statistics(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderProfileStatistics, ProviderAdminError> {
        if self.account(account_id).await?.authentication_kind()
            != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
        }
        let statistics = self
            .profile_statistics
            .profile_statistics(account_id)
            .await
            .map_err(map_profile_statistics_error)?;
        Ok(project_profile_statistics(statistics))
    }

    async fn profile_avatar(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderProfileAvatar, ProviderAdminError> {
        if self.account(account_id).await?.authentication_kind()
            != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
        }
        self.profile_statistics
            .profile_avatar(account_id)
            .await
            .map(project_profile_avatar)
            .map_err(map_profile_avatar_error)
    }

    async fn reset_credits(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderResetCredits, ProviderAdminError> {
        if self.account(account_id).await?.authentication_kind()
            != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
        }
        self.quota
            .list_reset_credits(account_id)
            .await
            .map(project_reset_credits)
            .map_err(map_reset_credits_error)
    }

    async fn consume_reset_credit(
        &self,
        command: ConsumeProviderResetCredit,
    ) -> Result<ProviderResetCreditResult, ProviderAdminError> {
        if self
            .account(&command.account_id)
            .await?
            .authentication_kind()
            != crate::credential::CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported));
        }
        self.quota
            .consume_reset_credit(
                &command.account_id,
                command.credit_id.as_deref(),
                command.redeem_request_id,
            )
            .await
            .map(|result| ProviderResetCreditResult {
                code: result.code,
                credit: result.credit.map(project_reset_credit),
            })
            .map_err(map_reset_credits_error)
    }

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        let account = self.account(account_id).await?;
        let catalog = if refresh {
            self.catalog
                .refresh_account_catalog(account_id)
                .await
                .map_err(map_catalog_error)?
        } else {
            self.catalog
                .cached_or_refresh_account_catalog(&account)
                .await
                .map_err(map_catalog_error)?
        };
        let models = catalog
            .models()
            .iter()
            .cloned()
            .map(|model| {
                let id = UpstreamModelId::new(model.clone())
                    .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?;
                Ok(ProviderModel { id, name: model })
            })
            .collect::<Result<Vec<_>, ProviderAdminError>>()?;
        Ok(ProviderModels {
            models,
            observed_at: Some(catalog.observed_at()),
        })
    }

    async fn model_catalog_document(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderModelCatalogDocument, ProviderAdminError> {
        let account = self.account(account_id).await?;
        let (models, observed_at) = self
            .catalog
            .account_catalog_documents(&account)
            .await
            .map_err(map_catalog_error)?;
        // 只有 Codex 原生对象带齐推理强度、上下文窗口等元数据；API 目录只有模型 ID，
        // 拼出来的文件不满足 Codex `model_catalog_json` 的加载要求，这里直接拒绝而不降格
        let mut entries = Vec::with_capacity(models.len());
        for model in &models {
            if model.document().protocol() != "codex" {
                return Err(provider_admin_error(ProviderAdminErrorKind::Unsupported)
                    .with_public_message("该账号没有可导出的模型目录"));
            }
            let entry: serde_json::Value = serde_json::from_slice(model.document().body())
                .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?;
            entries.push(entry);
        }
        let model_count = entries.len();
        let body = serde_json::to_vec(&serde_json::json!({ "models": entries }))
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?;
        let document = RawJsonPayload::new("codex", Bytes::from(body))
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?;
        Ok(ProviderModelCatalogDocument {
            document,
            model_count,
            observed_at: DateTime::<Utc>::from(observed_at),
        })
    }

    async fn export_credentials(
        &self,
        credentials: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError> {
        let mut account_ids = Vec::with_capacity(credentials.len());
        let mut items = Vec::with_capacity(credentials.len());
        for input in credentials {
            validate_account_record(&input.account, &self.provider_kind)?;
            let current = LoadedCredential {
                account: account_from_record(&input.account)?,
                credential: PlaintextCredential::new(
                    input.provider_material.into_provider_data().into_inner(),
                ),
            };
            account_ids.push(current.account.id().clone());
            items.push(ExportManagedCodexCredential {
                current,
                created_at: input.account.created_at,
                updated_at: input.account.updated_at,
            });
        }
        let document = CodexCredentialAdmin
            .format_cpr_export(items)
            .and_then(|document| document.into_json())
            .map_err(map_credential_admin_error)?;
        let Value::Object(document) = document else {
            return Err(provider_admin_error(ProviderAdminErrorKind::Internal));
        };
        Ok(ProviderExport {
            provider_kind: self.provider_kind.clone(),
            account_ids,
            document: ProviderDocument::new(OpaqueProviderData::new(document)),
        })
    }
}
fn build_connection_test_operation(
    upstream_model: &UpstreamModelId,
    input_text: &str,
) -> Result<Operation, ProviderAdminError> {
    let mut body = Map::new();
    body.insert(
        "model".to_owned(),
        Value::String(upstream_model.as_str().to_owned()),
    );
    body.insert(
        "input".to_owned(),
        serde_json::json!([{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": input_text}]
        }]),
    );
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert("store".to_owned(), Value::Bool(false));
    let payload = ProtocolPayload::json_object(OPENAI_PROTOCOL, body)
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    Ok(Operation::Generate(GenerateRequest::from_protocol_payload(
        payload,
    )))
}

fn currency_cost(money: Money) -> Result<CurrencyCost, ProviderAdminError> {
    Ok(CurrencyCost {
        currency: money.currency().as_str().to_owned(),
        amount: money
            .amount()
            .to_string()
            .parse::<DecimalAmount>()
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?,
    })
}

impl OpenAiAdminProvider {
    pub(super) async fn read_quota_snapshots(
        &self,
        requests: &[ProviderQuotaReadRequest<'_>],
    ) -> Vec<Result<ProviderQuota, ProviderAdminError>> {
        let identities = requests
            .iter()
            .map(|request| {
                if request.account.provider_kind != self.provider_kind {
                    return Err(provider_admin_error(ProviderAdminErrorKind::NotFound));
                }
                let id = ProviderAccountId::new(request.account.id.clone())
                    .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
                let revision =
                    CredentialRevision::new(request.account.credential_revision.get())
                        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
                Ok((id, revision))
            })
            .collect::<Vec<_>>();
        let ids = requests
            .iter()
            .zip(&identities)
            .filter(|(request, _)| {
                request.account.authentication_kind
                    != crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY
            })
            .filter_map(|(_, identity)| identity.as_ref().ok().map(|(id, _)| id.clone()))
            .collect::<Vec<_>>();
        let observations = if ids.is_empty() {
            Ok(BTreeMap::new())
        } else {
            self.accounts
                .get_quotas(&ids)
                .await
                .map_err(|error| map_quota_error(error.into()))
                .map(|values| {
                    values
                        .into_iter()
                        .map(|observation| (observation.account_id.clone(), observation))
                        .collect::<BTreeMap<_, _>>()
                })
        };
        requests
            .iter()
            .zip(identities)
            .map(|(request, identity)| {
                let (id, revision) = identity?;
                let snapshot = if request.account.authentication_kind
                    == crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY
                {
                    return Ok(empty_quota());
                } else {
                    observations
                        .as_ref()
                        .map_err(Clone::clone)?
                        .get(&id)
                        .map(|observation| {
                            self.quota.project_observation(&id, revision, observation)
                        })
                        .transpose()
                        .map_err(map_quota_error)?
                        .flatten()
                };
                Ok(project_quota(
                    snapshot,
                    request.account.quota.is_exhausted(),
                    &request.account.quota_settings,
                ))
            })
            .collect()
    }
}
