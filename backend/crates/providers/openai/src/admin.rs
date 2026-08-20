//! OpenAI 管理边界：Provider preparation 与 Redis OAuth pending 适配。

use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::model::accounts::AccountRecord;
use gateway_admin::model::observability::{
    CalculatedBillingBreakdown, CurrencyCost, DashboardDesktopRelease, DashboardWireAttribute,
    DashboardWireProfile, DashboardWireTarget, DecimalAmount, DesktopReleaseStatus,
    ProviderBillingInput,
};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwner, AuthorizationOwnerBinding,
    AuthorizationStarted, CompleteAuthorization, CredentialCommitGuard,
    PendingAuthorizationMutation, PrepareCredentialImport, PrepareCredentialRefresh,
    PrepareCredentialRotation, PreparedAuthorizationCommit, PreparedAuthorizationCredential,
    PreparedCredentialCreate, PreparedCredentialImport, PreparedCredentialRotation,
    PreparedCredentialRotationFacts, ProviderDocument, ProviderExport,
    ProviderExportCredentialInput, ProviderModel, ProviderModels, ProviderQuota,
    ProviderQuotaRequest, ProviderQuotaWindow, ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_admin::ports::provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::accounting::Money;
use gateway_core::engine::credential::{
    CredentialCasUpdateParts, CredentialRevision, LoadedCredential, NewProviderAccount,
    OpaqueProviderData, PlaintextCredential, ProviderAccount, ProviderAccountId,
    ProviderAccountStore,
};
use gateway_core::error::StoreErrorKind;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingClaimOutcome, OAuthPendingConsumeOutcome,
    OAuthPendingFlowPort, OAuthPendingPutOutcome, OAuthPendingReleaseOutcome, ProviderStoreError,
    ProviderStoreErrorKind,
};
use gateway_core::routing::{ProviderKind, UpstreamModelId};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::{Map, Number, Value};

use crate::credential::{
    CodexAccountQuotaSnapshot, CodexCredentialAdmin, CodexCredentialAdminError,
    CodexCredentialAdminService, CodexCredentialCatalogError, CodexCredentialCatalogService,
    CodexCredentialQuotaError, CodexCredentialQuotaService, CodexOAuthAdmin, CodexOAuthAdminError,
    CodexOAuthPendingClaimOutcome, CodexOAuthPendingStore, CodexOAuthPendingStoreError,
    CodexPendingAuthorization, CodexQuotaWindow, CodexQuotaWindowKind, CodexQuotaWindowRole,
    CompleteCodexOAuthAuthorization, CompletedCodexOAuthCredential, ExportManagedCodexCredential,
    StartCodexOAuthAuthorization, StoredCodexPendingAuthorization,
};
use crate::credential::{
    CodexCredentialCodec, CodexOAuthSecret, oauth_owner_ref, parse_access_token_expiration,
};
use crate::transport::CodexWebSocketPool;
use crate::transport::profile::{
    CodexDesktopReleaseSnapshot, CodexDesktopReleaseStatus, CodexWireProfile, CodexWireProfileState,
};
use crate::transport::{OpenAiBillingUsage, openai_billing_breakdown};

const PROVIDER_NAME: &str = "openai";
const PENDING_DOCUMENT_SCHEMA_VERSION: u64 = 3;

/// OpenAI 对终态 Admin port 的唯一实现。
pub(crate) struct OpenAiAdminProvider {
    provider_kind: ProviderKind,
    profile: CodexWireProfileState,
    accounts: Arc<dyn ProviderAccountStore>,
    credentials: Arc<CodexCredentialAdminService>,
    oauth: Arc<dyn CodexOAuthAdmin>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
    websocket_pool: Arc<CodexWebSocketPool>,
    desktop_release: CodexDesktopReleaseStatus,
}

pub(crate) struct OpenAiAdminServices {
    pub(crate) credentials: Arc<CodexCredentialAdminService>,
    pub(crate) oauth: Arc<dyn CodexOAuthAdmin>,
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
    fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    async fn account_unavailable(&self, account_id: &ProviderAccountId) {
        self.websocket_pool.evict_account(account_id.as_str()).await;
    }

    async fn account_facts_changed(&self, account_ids: &[ProviderAccountId]) {
        if account_ids.is_empty() {
            return;
        }
        self.quota.invalidate_scheduling(account_ids);
        if let Err(error) = self.catalog.invalidate() {
            tracing::warn!(
                account_count = account_ids.len(),
                error = %error,
                "OpenAI model catalog invalidation failed after account commit"
            );
        }
    }

    fn connection_test_operation(
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
                label: "客户端标识".to_owned(),
                value: client_identity,
            }],
            verified_at: Some(profile.verified_at),
            release: Some(release),
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
                input.cached_tokens.unwrap_or_default(),
                input.cache_write_tokens.unwrap_or_default(),
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

    async fn prepare_import(
        &self,
        command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        let prepared = self
            .credentials
            .prepare_import_document(Value::Object(
                command.document.into_provider_data().into_inner(),
            ))
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
        pending: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        if pending.provider_kind() != &self.provider_kind {
            return Err(provider_admin_error(ProviderAdminErrorKind::Invalid));
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
            ) => {
                prepared_create(credential, Utc::now()).map(PreparedAuthorizationCredential::Create)
            }
            (
                AuthorizationMutationTarget::Reauthorize { .. },
                CompletedCodexOAuthCredential::Reauthorize(credential),
            ) => prepared_rotation(credential, mutation.provider_kind().clone())
                .map(PreparedAuthorizationCredential::Reauthorize),
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
            return Err(provider_admin_error(ProviderAdminErrorKind::Conflict));
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
            .prepare_refreshed_oauth_rotation(current, secret, access_token_expires_at, None)
            .map_err(map_credential_admin_error)?;
        prepared_rotation(prepared, command.account.provider_kind)
    }

    async fn prepare_refresh(
        &self,
        command: PrepareCredentialRefresh,
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
            return Err(provider_admin_error(ProviderAdminErrorKind::Conflict));
        }
        let prepared = self
            .credentials
            .manual_refresh(current)
            .await
            .map_err(map_credential_admin_error)?;
        prepared_rotation(prepared, command.account.provider_kind)
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
                .read_account(&account_id)
                .await
                .map_err(map_quota_error)?
        };
        Ok(project_quota(snapshot, &account))
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
                added_at: input.account.created_at,
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

struct OpenAiCredentialCommitGuard {
    _guard: crate::credential::PreparedCodexCredentialRotationGuard,
}

impl CredentialCommitGuard for OpenAiCredentialCommitGuard {
    fn finish(self: Box<Self>) {}
}

fn prepared_create(
    prepared: NewProviderAccount,
    observed_at: DateTime<Utc>,
) -> Result<PreparedCredentialCreate, ProviderAdminError> {
    let NewProviderAccount {
        account,
        credential,
    } = prepared;
    Ok(PreparedCredentialCreate {
        account_id: account.id().clone(),
        provider_kind: account.provider().clone(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        authentication_kind: account.authentication_kind().to_owned(),
        provider_material: ProviderDocument::new(OpaqueProviderData::new(credential.into_inner())),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: account.access_token_expires_at().map(DateTime::<Utc>::from),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        credential_state: account.credential_state(),
        credential_observed_at: observed_at,
    })
}

fn prepared_rotation(
    prepared: crate::credential::PreparedCodexCredentialRotation,
    provider_kind: ProviderKind,
) -> Result<PreparedCredentialRotation, ProviderAdminError> {
    let (profile, credential, replacement_identity, guard) = prepared.into_parts();
    let CredentialCasUpdateParts {
        account_id,
        expected_revision,
        profile: credential_profile,
        credential,
        has_refresh_token,
        access_token_expires_at,
        next_refresh_at,
        account_state: _account_state,
    } = credential.into_parts();
    if profile != credential_profile || profile.account_id != account_id {
        return Err(provider_admin_error(ProviderAdminErrorKind::Internal));
    }
    let expected_credential_revision = Revision::new(expected_revision.get())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Internal))?;
    Ok(PreparedCredentialRotation::new(
        PreparedCredentialRotationFacts {
            account_id,
            provider_kind,
            expected_credential_revision,
            replacement_identity,
            name: profile.name,
            email: profile.email,
            plan_type: profile.plan_type,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(
                credential.into_inner(),
            )),
            has_refresh_token,
            access_token_expires_at: access_token_expires_at.map(DateTime::<Utc>::from),
            next_refresh_at: next_refresh_at.map(DateTime::<Utc>::from),
        },
        Box::new(OpenAiCredentialCommitGuard { _guard: guard }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RotationDocument {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

fn rotation_secret(document: ProviderDocument) -> Result<CodexOAuthSecret, ProviderAdminError> {
    let document: RotationDocument =
        serde_json::from_value(Value::Object(document.into_provider_data().into_inner()))
            .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    Ok(CodexOAuthSecret {
        access_token: SecretString::from(document.access_token),
        refresh_token: document.refresh_token.map(SecretString::from),
        id_token: document.id_token.map(SecretString::from),
    })
}

fn validate_account_record(
    account: &AccountRecord,
    provider_kind: &ProviderKind,
) -> Result<(), ProviderAdminError> {
    if &account.provider_kind != provider_kind || provider_kind.as_str() != PROVIDER_NAME {
        return Err(provider_admin_error(ProviderAdminErrorKind::Invalid));
    }
    ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    Ok(())
}

fn account_from_record(account: &AccountRecord) -> Result<ProviderAccount, ProviderAdminError> {
    let account_id = ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    let revision = CredentialRevision::new(account.credential_revision.get())
        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
    Ok(ProviderAccount::new(
        account_id,
        account.provider_kind.clone(),
        account.name.clone(),
        account.upstream_user_id.clone(),
        account.authentication_kind.clone(),
        revision,
        account.access_token_expires_at.map(SystemTime::from),
    )
    .with_profile(
        account.email.clone(),
        account.upstream_account_id.clone(),
        account.plan_type.clone(),
    )
    .with_account_facts(
        account.enabled,
        account.credential_state,
        account.quota,
        account.last_error_reason,
        account.last_error_message.clone(),
    )
    .with_refresh_schedule(
        account.has_refresh_token,
        account.next_refresh_at.map(SystemTime::from),
    ))
}

fn dashboard_desktop_release(
    profile: &CodexWireProfile,
    snapshot: CodexDesktopReleaseSnapshot,
) -> DashboardDesktopRelease {
    let status = if snapshot.checked_at.is_none() {
        DesktopReleaseStatus::Unchecked
    } else if snapshot.last_error.is_some() {
        DesktopReleaseStatus::Failed
    } else if snapshot.latest.as_ref().is_some_and(|latest| {
        latest.version == profile.desktop_version && latest.build == profile.desktop_build
    }) {
        DesktopReleaseStatus::Current
    } else if snapshot.latest.is_some() {
        DesktopReleaseStatus::UpdateAvailable
    } else {
        DesktopReleaseStatus::Failed
    };
    let latest = snapshot.latest;
    DashboardDesktopRelease {
        status,
        checked_at: snapshot.checked_at,
        latest_version: latest.as_ref().map(|release| release.version.clone()),
        latest_build: latest.as_ref().map(|release| release.build.clone()),
        published_at: latest.as_ref().and_then(|release| release.published_at),
        minimum_system_version: latest
            .as_ref()
            .and_then(|release| release.minimum_system_version.clone()),
        hardware_requirements: latest
            .as_ref()
            .and_then(|release| release.hardware_requirements.clone()),
        download_url: latest
            .as_ref()
            .and_then(|release| release.download_url.clone()),
        download_size: latest.as_ref().and_then(|release| release.download_size),
        signature_present: latest.as_ref().map(|release| release.signature_present),
        error: snapshot.last_error,
    }
}

fn account_matches_record(account: &ProviderAccount, record: &AccountRecord) -> bool {
    account.id().as_str() == record.id
        && account.provider() == &record.provider_kind
        && account.upstream_user_id() == record.upstream_user_id.as_deref()
        && account.upstream_account_id() == record.upstream_account_id.as_deref()
        && account.authentication_kind() == record.authentication_kind
}

fn empty_quota() -> ProviderQuota {
    ProviderQuota {
        observed_at: None,
        refresh_token_expires_at: None,
        windows: Vec::new(),
        limit_reached: false,
        provider_data: None,
    }
}

fn project_quota(
    snapshot: Option<CodexAccountQuotaSnapshot>,
    account: &ProviderAccount,
) -> ProviderQuota {
    let mut quota = snapshot
        .map(project_quota_snapshot)
        .unwrap_or_else(empty_quota);
    quota.limit_reached = account.quota().is_exhausted();
    quota
}

fn project_quota_snapshot(snapshot: CodexAccountQuotaSnapshot) -> ProviderQuota {
    let mut provider_data = Map::new();
    provider_data.insert(
        "remaining_percent".to_owned(),
        snapshot
            .fact()
            .remaining_percent()
            .map_or(Value::Null, |value| Value::Number(Number::from(value))),
    );
    provider_data.insert(
        "exhausted".to_owned(),
        Value::Bool(snapshot.quota().is_exhausted()),
    );
    let windows: Vec<ProviderQuotaWindow> = snapshot
        .windows()
        .iter()
        .filter(|window| should_project_quota_window(window))
        .map(|window| ProviderQuotaWindow {
            key: window.key().to_owned(),
            group: quota_group(window.kind()).to_owned(),
            label: codex_quota_window_label(window.kind(), window.role(), window.window_seconds()),
            limit_id: Some(window.source().to_owned()),
            limit_name: window.limit_name().map(str::to_owned),
            role: Some(quota_role(window.role())),
            local_usage_attribution: if window.is_account_wide() {
                QuotaLocalUsageAttribution::AccountWide
            } else {
                QuotaLocalUsageAttribution::Unavailable
            },
            window_seconds: window.window_seconds(),
            used_percent: window.used_percent(),
            reset_at: window.reset_at(),
            limit_reached: window.limit_reached(),
            local_usage: None,
            provider_data: None,
        })
        .collect();
    // 快照级 limit_reached 只看滚动后的窗口触顶：顶层标记是观测事实，不能
    // 在窗口全部过期后继续维持限流。
    let limit_reached = quota_windows_limit_reached(&windows);
    ProviderQuota {
        observed_at: Some(DateTime::<Utc>::from(snapshot.observed_at())),
        refresh_token_expires_at: None,
        windows,
        limit_reached,
        provider_data: Some(ProviderDocument::new(OpaqueProviderData::new(
            provider_data,
        ))),
    }
}

/// `secondary_window` 有时只是上游的空占位：没有时长、重置时间，也没有用量。
/// 它不能被可靠翻译成 5 小时或周额度，因此不应占用账号面板；带有实际事实的
/// 次级窗口（时长、重置、非零用量或触顶）仍完整保留。
fn should_project_quota_window(window: &CodexQuotaWindow) -> bool {
    window.role() != CodexQuotaWindowRole::Secondary
        || window.window_seconds().is_some()
        || window.reset_at().is_some()
        || window.used_percent().is_some_and(|used| used > 0.0)
        || window.limit_reached()
}

fn quota_windows_limit_reached(windows: &[ProviderQuotaWindow]) -> bool {
    windows.iter().any(|window| window.limit_reached)
}

const fn quota_group(kind: CodexQuotaWindowKind) -> &'static str {
    match kind {
        CodexQuotaWindowKind::Monthly => "monthly",
        CodexQuotaWindowKind::ShortTerm | CodexQuotaWindowKind::Weekly => "shortTerm",
        CodexQuotaWindowKind::Other => "other",
    }
}

const fn quota_role(role: CodexQuotaWindowRole) -> ProviderQuotaWindowRole {
    match role {
        CodexQuotaWindowRole::Primary => ProviderQuotaWindowRole::Primary,
        CodexQuotaWindowRole::Secondary => ProviderQuotaWindowRole::Secondary,
        CodexQuotaWindowRole::Monthly => ProviderQuotaWindowRole::Monthly,
    }
}

/// 按窗口时长显示汉化额度名；额度桶名称通过独立字段投影。
fn codex_quota_window_label(
    kind: CodexQuotaWindowKind,
    role: CodexQuotaWindowRole,
    window_seconds: Option<u64>,
) -> String {
    match kind {
        CodexQuotaWindowKind::Monthly => "月额度".to_owned(),
        CodexQuotaWindowKind::Weekly => "周额度".to_owned(),
        CodexQuotaWindowKind::ShortTerm => {
            if window_seconds.is_some_and(|seconds| seconds > 86_400) {
                "周额度".to_owned()
            } else {
                "5小时额度".to_owned()
            }
        }
        CodexQuotaWindowKind::Other => custom_quota_window_label(window_seconds, role),
    }
}

fn custom_quota_window_label(window_seconds: Option<u64>, role: CodexQuotaWindowRole) -> String {
    let Some(seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        // 与官方客户端一致：没有时长时不臆测为 5 小时或周额度；保留
        // primary/secondary 语义，让用户知道这是上游未标明时长的独立窗口。
        return match role {
            CodexQuotaWindowRole::Primary => "主额度".to_owned(),
            CodexQuotaWindowRole::Secondary => "次级额度".to_owned(),
            CodexQuotaWindowRole::Monthly => "月额度".to_owned(),
        };
    };
    if seconds % 86_400 == 0 {
        format!("{}日额度", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}小时额度", seconds / 3_600)
    } else {
        format!("{}分钟额度", seconds.div_ceil(60))
    }
}

/// 将 Provider-owned PKCE/OIDC 状态保存到 Store 提供的 Redis 原子端口。
pub(crate) struct OpenAiOAuthPendingStore {
    port: Arc<dyn OAuthPendingFlowPort>,
    provider_kind: ProviderKind,
}

impl OpenAiOAuthPendingStore {
    pub(crate) const fn new(
        port: Arc<dyn OAuthPendingFlowPort>,
        provider_kind: ProviderKind,
    ) -> Self {
        Self {
            port,
            provider_kind,
        }
    }
}

#[async_trait]
impl CodexOAuthPendingStore for OpenAiOAuthPendingStore {
    async fn create(
        &self,
        pending: &CodexPendingAuthorization,
    ) -> Result<(), CodexOAuthPendingStoreError> {
        let now = Utc::now();
        let ttl = (pending.expires_at() - now)
            .to_std()
            .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?;
        let flow = NewOAuthPendingFlow::try_new(
            self.provider_kind.clone(),
            binding(pending.flow_id())?,
            binding(pending.owner_ref())?,
            ttl,
            OpaqueProviderData::new(encode_pending(pending)),
        )
        .map_err(map_pending_store_error)?;
        match self
            .port
            .put_if_absent(flow)
            .await
            .map_err(map_pending_store_error)?
        {
            OAuthPendingPutOutcome::Stored => Ok(()),
            OAuthPendingPutOutcome::AlreadyExists => Err(CodexOAuthPendingStoreError::Conflict),
        }
    }

    async fn claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
        claim_ttl: std::time::Duration,
    ) -> Result<CodexOAuthPendingClaimOutcome, CodexOAuthPendingStoreError> {
        let flow = binding(flow_id)?;
        let owner = binding(owner_ref)?;
        let claim = binding(claim_ref)?;
        let outcome = self
            .port
            .claim_if_owner(&self.provider_kind, &flow, &owner, &claim, claim_ttl)
            .await
            .map_err(map_pending_store_error)?;
        match outcome {
            OAuthPendingClaimOutcome::Claimed(payload) => match decode_pending(payload) {
                Ok(pending) => Ok(CodexOAuthPendingClaimOutcome::Claimed(Box::new(pending))),
                Err(error) => match self
                    .port
                    .release_claim(&self.provider_kind, &flow, &owner, &claim)
                    .await
                    .map_err(map_pending_store_error)?
                {
                    OAuthPendingReleaseOutcome::Released => Err(error),
                    OAuthPendingReleaseOutcome::NotFound
                    | OAuthPendingReleaseOutcome::OwnerMismatch
                    | OAuthPendingReleaseOutcome::ClaimMismatch => {
                        Err(CodexOAuthPendingStoreError::Unavailable)
                    }
                },
            },
            OAuthPendingClaimOutcome::NotFound | OAuthPendingClaimOutcome::OwnerMismatch => {
                Ok(CodexOAuthPendingClaimOutcome::NotFound)
            }
            OAuthPendingClaimOutcome::InProgress => Ok(CodexOAuthPendingClaimOutcome::InProgress),
        }
    }

    async fn release_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        let flow = binding(flow_id)?;
        let owner = binding(owner_ref)?;
        let claim = binding(claim_ref)?;
        match self
            .port
            .release_claim(&self.provider_kind, &flow, &owner, &claim)
            .await
            .map_err(map_pending_store_error)?
        {
            OAuthPendingReleaseOutcome::Released => Ok(true),
            OAuthPendingReleaseOutcome::NotFound
            | OAuthPendingReleaseOutcome::OwnerMismatch
            | OAuthPendingReleaseOutcome::ClaimMismatch => Ok(false),
        }
    }

    async fn consume_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError> {
        let flow = binding(flow_id)?;
        let owner = binding(owner_ref)?;
        let claim = binding(claim_ref)?;
        match self
            .port
            .consume_claim(&self.provider_kind, &flow, &owner, &claim)
            .await
            .map_err(map_pending_store_error)?
        {
            OAuthPendingConsumeOutcome::Consumed => Ok(true),
            OAuthPendingConsumeOutcome::NotFound
            | OAuthPendingConsumeOutcome::OwnerMismatch
            | OAuthPendingConsumeOutcome::ClaimMismatch => Ok(false),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingDocument {
    flow_id: String,
    owner_ref: String,
    started_request_ref: String,
    name: String,
    expires_at: DateTime<Utc>,
    state: String,
    nonce: String,
    code_verifier: String,
    installation_id: String,
    reauthorization_account_id: Option<String>,
    mutation: PendingMutationDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingMutationDocument {
    schema_version: u64,
    provider_kind: String,
    target: PendingTargetDocument,
    owner: PendingOwnerDocument,
    started_request_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PendingTargetDocument {
    Create { name: String },
    Reauthorize { account_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PendingOwnerDocument {
    AdminSession { admin_user_id: String },
    AdminApiKey,
    System,
}

fn encode_pending(pending: &CodexPendingAuthorization) -> Map<String, Value> {
    let mut document = Map::new();
    document.insert(
        "flow_id".to_owned(),
        Value::String(pending.flow_id().to_owned()),
    );
    document.insert(
        "owner_ref".to_owned(),
        Value::String(pending.owner_ref().to_owned()),
    );
    document.insert(
        "started_request_ref".to_owned(),
        Value::String(pending.started_request_ref().to_owned()),
    );
    document.insert("name".to_owned(), Value::String(pending.name().to_owned()));
    document.insert(
        "expires_at".to_owned(),
        Value::String(pending.expires_at().to_rfc3339()),
    );
    document.insert(
        "state".to_owned(),
        Value::String(pending.state().expose_secret().to_owned()),
    );
    document.insert(
        "nonce".to_owned(),
        Value::String(pending.nonce().expose_secret().to_owned()),
    );
    document.insert(
        "code_verifier".to_owned(),
        Value::String(pending.code_verifier().expose_secret().to_owned()),
    );
    document.insert(
        "installation_id".to_owned(),
        Value::String(pending.installation_id().to_owned()),
    );
    document.insert(
        "reauthorization_account_id".to_owned(),
        pending
            .reauthorization()
            .map_or(Value::Null, |target| Value::String(target.to_string())),
    );
    document.insert(
        "mutation".to_owned(),
        Value::Object(encode_mutation(pending.mutation())),
    );
    document
}

fn encode_mutation(mutation: &PendingAuthorizationMutation) -> Map<String, Value> {
    let mut document = Map::new();
    document.insert(
        "schema_version".to_owned(),
        Value::Number(Number::from(PENDING_DOCUMENT_SCHEMA_VERSION)),
    );
    document.insert(
        "provider_kind".to_owned(),
        Value::String(mutation.provider_kind().as_str().to_owned()),
    );
    document.insert(
        "target".to_owned(),
        Value::Object(encode_target(mutation.target())),
    );
    document.insert(
        "owner".to_owned(),
        Value::Object(encode_owner(mutation.owner_binding().owner())),
    );
    document.insert(
        "started_request_id".to_owned(),
        Value::String(mutation.owner_binding().started_request_id().to_owned()),
    );
    document
}

fn encode_target(target: &AuthorizationMutationTarget) -> Map<String, Value> {
    let mut document = Map::new();
    match target {
        AuthorizationMutationTarget::Create { name } => {
            document.insert("kind".to_owned(), Value::String("create".to_owned()));
            document.insert("name".to_owned(), Value::String(name.clone()));
        }
        AuthorizationMutationTarget::Reauthorize { account_id } => {
            document.insert("kind".to_owned(), Value::String("reauthorize".to_owned()));
            document.insert(
                "account_id".to_owned(),
                Value::String(account_id.to_string()),
            );
        }
    }
    document
}

fn encode_owner(owner: &AuthorizationOwner) -> Map<String, Value> {
    let mut document = Map::new();
    match owner {
        AuthorizationOwner::AdminSession { admin_user_id } => {
            document.insert("kind".to_owned(), Value::String("admin_session".to_owned()));
            document.insert(
                "admin_user_id".to_owned(),
                Value::String(admin_user_id.clone()),
            );
        }
        AuthorizationOwner::AdminApiKey => {
            document.insert("kind".to_owned(), Value::String("admin_api_key".to_owned()));
        }
        AuthorizationOwner::System => {
            document.insert("kind".to_owned(), Value::String("system".to_owned()));
        }
    }
    document
}

fn decode_pending(
    payload: OpaqueProviderData,
) -> Result<CodexPendingAuthorization, CodexOAuthPendingStoreError> {
    let document: PendingDocument = serde_json::from_value(Value::Object(payload.into_inner()))
        .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?;
    CodexPendingAuthorization::from_stored(StoredCodexPendingAuthorization {
        flow_id: document.flow_id,
        owner_ref: document.owner_ref,
        started_request_ref: document.started_request_ref,
        name: document.name,
        expires_at: document.expires_at,
        state: SecretString::from(document.state),
        nonce: SecretString::from(document.nonce),
        code_verifier: SecretString::from(document.code_verifier),
        installation_id: document.installation_id,
        reauthorization_account_id: document.reauthorization_account_id,
        mutation: decode_mutation(document.mutation)?,
    })
}

fn decode_mutation(
    document: PendingMutationDocument,
) -> Result<PendingAuthorizationMutation, CodexOAuthPendingStoreError> {
    if document.schema_version != PENDING_DOCUMENT_SCHEMA_VERSION {
        return Err(CodexOAuthPendingStoreError::InvalidValue);
    }
    let provider_kind = ProviderKind::new(document.provider_kind)
        .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?;
    let target = match document.target {
        PendingTargetDocument::Create { name } => AuthorizationMutationTarget::Create { name },
        PendingTargetDocument::Reauthorize { account_id } => {
            AuthorizationMutationTarget::Reauthorize {
                account_id: ProviderAccountId::new(account_id)
                    .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?,
            }
        }
    };
    let actor = match document.owner {
        PendingOwnerDocument::AdminSession { admin_user_id } => {
            MutationActor::AdminSession { admin_user_id }
        }
        PendingOwnerDocument::AdminApiKey => MutationActor::AdminApiKey,
        PendingOwnerDocument::System => MutationActor::System,
    };
    let context = MutationContext {
        actor,
        request_id: document.started_request_id,
    };
    Ok(PendingAuthorizationMutation::new(
        provider_kind,
        target,
        AuthorizationOwnerBinding::from_context(&context),
    ))
}

fn binding(value: &str) -> Result<OAuthPendingBinding, CodexOAuthPendingStoreError> {
    OAuthPendingBinding::try_new(value.to_owned()).map_err(map_pending_store_error)
}

fn provider_admin_error(kind: ProviderAdminErrorKind) -> ProviderAdminError {
    ProviderAdminError::new(kind)
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
    let payload = ProtocolPayload::json_object("openai", body)
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

fn map_pending_store_error(error: ProviderStoreError) -> CodexOAuthPendingStoreError {
    match error.kind() {
        ProviderStoreErrorKind::InvalidData => CodexOAuthPendingStoreError::InvalidValue,
        ProviderStoreErrorKind::Conflict => CodexOAuthPendingStoreError::Conflict,
        ProviderStoreErrorKind::Unavailable => CodexOAuthPendingStoreError::Unavailable,
    }
}

fn map_store_error(error: gateway_core::error::StoreError) -> ProviderAdminError {
    provider_admin_error(match error.kind() {
        StoreErrorKind::Conflict => ProviderAdminErrorKind::Conflict,
        StoreErrorKind::InvalidData | StoreErrorKind::InvalidState => {
            ProviderAdminErrorKind::NotFound
        }
        StoreErrorKind::Unavailable => ProviderAdminErrorKind::Unavailable,
        _ => ProviderAdminErrorKind::Internal,
    })
}

fn map_credential_admin_error(error: CodexCredentialAdminError) -> ProviderAdminError {
    use CodexCredentialAdminError as Error;
    let upstream_message = error.upstream_message().map(ToOwned::to_owned);
    let kind = match error {
        Error::InvalidInput
        | Error::InvalidCredential
        | Error::MissingRefreshToken
        | Error::RefreshRejected { .. }
        | Error::AccountBanned { .. } => ProviderAdminErrorKind::Invalid,
        Error::NotFound => ProviderAdminErrorKind::NotFound,
        Error::RefreshLeaseUnavailable | Error::RefreshAmbiguous { .. } => {
            ProviderAdminErrorKind::Conflict
        }
        Error::RefreshUnavailable => ProviderAdminErrorKind::Unavailable,
    };
    let error = provider_admin_error(kind);
    match upstream_message {
        Some(message) => error.with_message(message),
        None => error,
    }
}

const fn credential_admin_error_code(error: &CodexCredentialAdminError) -> &'static str {
    match error {
        CodexCredentialAdminError::InvalidInput => "invalid_input",
        CodexCredentialAdminError::InvalidCredential => "invalid_credential",
        CodexCredentialAdminError::NotFound => "not_found",
        CodexCredentialAdminError::MissingRefreshToken => "missing_refresh_token",
        CodexCredentialAdminError::RefreshLeaseUnavailable => "refresh_lease_unavailable",
        CodexCredentialAdminError::RefreshRejected { .. } => "refresh_rejected",
        CodexCredentialAdminError::AccountBanned { .. } => "account_banned",
        CodexCredentialAdminError::RefreshUnavailable => "refresh_unavailable",
        CodexCredentialAdminError::RefreshAmbiguous { .. } => "refresh_ambiguous",
    }
}

const fn provider_admin_error_code(kind: ProviderAdminErrorKind) -> &'static str {
    match kind {
        ProviderAdminErrorKind::Invalid => "invalid",
        ProviderAdminErrorKind::Unsupported => "unsupported",
        ProviderAdminErrorKind::NotFound => "not_found",
        ProviderAdminErrorKind::Conflict => "conflict",
        ProviderAdminErrorKind::Unavailable => "unavailable",
        ProviderAdminErrorKind::Internal => "internal",
    }
}

fn log_import_failure(stage: &'static str, error: &'static str) {
    tracing::warn!(
        import_stage = stage,
        import_error = error,
        "OpenAI credential import preparation failed"
    );
}

fn map_oauth_error(error: CodexOAuthAdminError) -> ProviderAdminError {
    use CodexOAuthAdminError as Error;
    provider_admin_error(match error {
        Error::InvalidInput
        | Error::CallbackRejected
        | Error::TokenRejected
        | Error::Credential => ProviderAdminErrorKind::Invalid,
        Error::NotFound | Error::FlowExpired => ProviderAdminErrorKind::NotFound,
        Error::Conflict | Error::Ambiguous => ProviderAdminErrorKind::Conflict,
        Error::UpstreamUnavailable | Error::StorageUnavailable => {
            ProviderAdminErrorKind::Unavailable
        }
    })
}

const fn oauth_error_code(error: &CodexOAuthAdminError) -> &'static str {
    match error {
        CodexOAuthAdminError::InvalidInput => "invalid_input",
        CodexOAuthAdminError::NotFound => "not_found",
        CodexOAuthAdminError::Conflict => "conflict",
        CodexOAuthAdminError::FlowExpired => "flow_expired",
        CodexOAuthAdminError::CallbackRejected => "callback_rejected",
        CodexOAuthAdminError::TokenRejected => "token_rejected",
        CodexOAuthAdminError::UpstreamUnavailable => "upstream_unavailable",
        CodexOAuthAdminError::Ambiguous => "ambiguous",
        CodexOAuthAdminError::StorageUnavailable => "storage_unavailable",
        CodexOAuthAdminError::Credential => "credential",
    }
}

fn map_quota_error(error: CodexCredentialQuotaError) -> ProviderAdminError {
    use CodexCredentialQuotaError as Error;
    provider_admin_error(match error {
        Error::InvalidCredentialData => ProviderAdminErrorKind::Invalid,
        Error::NotFound => ProviderAdminErrorKind::NotFound,
        Error::RevisionConflict => ProviderAdminErrorKind::Conflict,
        Error::CredentialRefreshRequired
        | Error::Repository(_)
        | Error::Store { .. }
        | Error::Upstream { .. } => ProviderAdminErrorKind::Unavailable,
    })
}

fn map_catalog_error(error: CodexCredentialCatalogError) -> ProviderAdminError {
    use CodexCredentialCatalogError as Error;
    provider_admin_error(match error {
        Error::InvalidCredentialData | Error::InvalidEtag => ProviderAdminErrorKind::Invalid,
        Error::NoEligibleCredential => ProviderAdminErrorKind::NotFound,
        Error::ConcurrentUpdate => ProviderAdminErrorKind::Conflict,
        Error::Upstream { .. } | Error::Cache => ProviderAdminErrorKind::Unavailable,
    })
}
