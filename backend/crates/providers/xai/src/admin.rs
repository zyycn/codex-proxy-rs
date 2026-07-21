//! xAI 管理边界：Provider preparation 与 Redis OAuth pending 适配。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::model::accounts::{
    AccountAvailability as AdminAccountAvailability, AccountRecord,
};
use gateway_admin::model::observability::{
    CalculatedBillingBreakdown, CurrencyCost, DashboardWireAttribute, DashboardWireProfile,
    DashboardWireTarget, DecimalAmount, ProviderBillingInput,
};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwner, AuthorizationOwnerBinding,
    AuthorizationStarted, CompleteAuthorization, CredentialCommitGuard,
    PendingAuthorizationMutation, PrepareCredentialImport, PrepareCredentialRefresh,
    PrepareCredentialRotation, PreparedAuthorizationCommit, PreparedAuthorizationCredential,
    PreparedCredentialCreate, PreparedCredentialImport, PreparedCredentialRotation,
    PreparedCredentialRotationFacts, ProviderDocument, ProviderExport,
    ProviderExportCredentialInput, ProviderModel, ProviderModels, ProviderQuota,
    ProviderQuotaRequest, ProviderQuotaWindow,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision};
use gateway_admin::ports::provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind};
use gateway_core::accounting::Money;
use gateway_core::engine::credential::{
    AccountAvailability, CredentialRevision, LoadedCredential, NewProviderAccount,
    OpaqueProviderData, PlaintextCredential, ProviderAccount, ProviderAccountId,
    ProviderAccountStore,
};
use gateway_core::error::StoreErrorKind;
use gateway_core::operation::{
    ContentPart, GenerateRequest, Message, MessageRole, Operation, ProtocolPayload,
    ResponsePersistence,
};
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingFlowPort, OAuthPendingPutOutcome,
    OAuthPendingTakeOutcome, ProviderRefreshPolicy, ProviderRuntimePolicyPort, ProviderStoreError,
    ProviderStoreErrorKind,
};
use gateway_core::routing::{ProviderInstanceId, ProviderKind, UpstreamModelId};
use serde::Deserialize;
use serde_json::{Map, Number, Value};
use sha2::{Digest as _, Sha256};
use url::Url;
use uuid::Uuid;

use crate::XaiWireProfileState;
use crate::credential::{
    AuthorizationCallback, FailureClass, GrokAccountProfile, GrokCredentialAdmin,
    GrokCredentialCatalogError, GrokCredentialCatalogService, GrokCredentialQuotaService,
    GrokCredentialRefreshError, GrokCredentialRefreshService, GrokCredentialRepository,
    GrokCredentialRepositoryError, GrokOAuthClient, GrokOAuthConfig, GrokOAuthImportCandidate,
    GrokOAuthImportDocument, GrokOAuthImportMetadata, GrokOAuthImportTokens, GrokOAuthSecret,
    GrokQuotaError, GrokQuotaPeriodKind, GrokQuotaSnapshot, OAuthError, PendingAuthorization,
    PreparedGrokCredentialRotation, PreparedGrokCredentialRotationGuard, RedirectUriAllowlist,
    RotateManagedGrokCredential, SecretValue, VerifiedGrokAccount, VerifiedTokenSet,
};
use crate::transport::{GROK_CLI_BASE_URL, XAI_PROVIDER_NAME, grok_billing_breakdown};

const PENDING_SCHEMA_VERSION: u64 = 1;
const PENDING_TTL: TimeDelta = TimeDelta::minutes(30);
const MAX_PENDING_TEXT_BYTES: usize = 512;

pub(crate) struct XaiAdminProvider {
    provider_kind: ProviderKind,
    wire_profile: XaiWireProfileState,
    accounts: Arc<dyn ProviderAccountStore>,
    repository: GrokCredentialRepository,
    oauth_config: GrokOAuthConfig,
    oauth: Arc<GrokOAuthClient>,
    pending: Arc<dyn OAuthPendingFlowPort>,
    refresh: Arc<GrokCredentialRefreshService>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
}

pub(crate) struct XaiAdminServices {
    pub(crate) repository: GrokCredentialRepository,
    pub(crate) oauth_config: GrokOAuthConfig,
    pub(crate) oauth: Arc<GrokOAuthClient>,
    pub(crate) pending: Arc<dyn OAuthPendingFlowPort>,
    pub(crate) refresh: Arc<GrokCredentialRefreshService>,
    pub(crate) quota: Arc<GrokCredentialQuotaService>,
    pub(crate) catalog: Arc<GrokCredentialCatalogService>,
    pub(crate) runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
}

impl XaiAdminProvider {
    #[must_use]
    pub(crate) fn new(
        provider_kind: ProviderKind,
        wire_profile: XaiWireProfileState,
        accounts: Arc<dyn ProviderAccountStore>,
        services: XaiAdminServices,
    ) -> Self {
        Self {
            provider_kind,
            wire_profile,
            accounts,
            repository: services.repository,
            oauth_config: services.oauth_config,
            oauth: services.oauth,
            pending: services.pending,
            refresh: services.refresh,
            quota: services.quota,
            catalog: services.catalog,
            runtime_policy: services.runtime_policy,
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
            .ok_or_else(|| provider_error(ProviderAdminErrorKind::NotFound))
    }

    async fn store_pending(
        &self,
        mutation: PendingAuthorizationMutation,
        pending: PendingAuthorization,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        let authorization_url = pending.authorization_url().to_string();
        let server_state = pending.into_server_state().map_err(map_oauth_error)?;
        let flow_id = random_flow_id()?;
        let expires_at = Utc::now()
            .checked_add_signed(PENDING_TTL)
            .ok_or_else(|| provider_error(ProviderAdminErrorKind::Internal))?;
        let owner_ref = owner_ref(mutation.owner_binding().owner());
        let ttl = (expires_at - Utc::now())
            .to_std()
            .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?;
        let payload = encode_pending(&StoredXaiAuthorization {
            flow_id: flow_id.clone(),
            owner_ref: owner_ref.clone(),
            expires_at,
            server_state: server_state.expose().to_owned(),
            mutation,
        });
        let flow = NewOAuthPendingFlow::try_new(
            self.provider_kind.clone(),
            binding(&flow_id)?,
            binding(&owner_ref)?,
            ttl,
            OpaqueProviderData::new(payload),
        )
        .map_err(map_provider_store_error)?;
        match self
            .pending
            .put_if_absent(flow)
            .await
            .map_err(map_provider_store_error)?
        {
            OAuthPendingPutOutcome::Stored => Ok(AuthorizationStarted {
                flow_id,
                authorization_url,
                expires_at,
            }),
            OAuthPendingPutOutcome::AlreadyExists => {
                Err(provider_error(ProviderAdminErrorKind::Conflict))
            }
        }
    }

    async fn take_pending(
        &self,
        context: &MutationContext,
        flow_id: &str,
    ) -> Result<StoredXaiAuthorization, ProviderAdminError> {
        let flow = binding(flow_id)?;
        let owner_binding = AuthorizationOwnerBinding::from_context(context);
        let owner_ref = owner_ref(owner_binding.owner());
        let owner = binding(&owner_ref)?;
        match self
            .pending
            .take_if_owner(&self.provider_kind, &flow, &owner)
            .await
            .map_err(map_provider_store_error)?
        {
            OAuthPendingTakeOutcome::Taken(payload) => {
                let stored = decode_pending(payload, &self.oauth_config)?;
                if stored.flow_id != flow_id || stored.owner_ref != owner_ref {
                    return Err(provider_error(ProviderAdminErrorKind::Invalid));
                }
                Ok(stored)
            }
            OAuthPendingTakeOutcome::NotFound | OAuthPendingTakeOutcome::OwnerMismatch => {
                Err(provider_error(ProviderAdminErrorKind::NotFound))
            }
        }
    }
}

#[async_trait]
impl ProviderAdmin for XaiAdminProvider {
    fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    async fn account_unavailable(&self, _account_id: &ProviderAccountId) {}

    fn connection_test_operation(
        &self,
        upstream_model: &UpstreamModelId,
        input_text: &str,
    ) -> Result<Operation, ProviderAdminError> {
        build_connection_test_operation(upstream_model, input_text)
    }

    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile> {
        Some(DashboardWireProfile {
            provider: self.provider_kind.as_str().to_owned(),
            product: "Grok Build".to_owned(),
            version: self.wire_profile.client_version(),
            build: None,
            target: DashboardWireTarget {
                os_type: self.wire_profile.target_os(),
                os_version: "—".to_owned(),
                arch: self.wire_profile.target_arch(),
                terminal: self.wire_profile.client_mode(),
            },
            user_agent: self.wire_profile.user_agent(),
            attributes: vec![
                DashboardWireAttribute {
                    label: "客户端标识".to_owned(),
                    value: self.wire_profile.client_identifier(),
                },
                DashboardWireAttribute {
                    label: "运行模式".to_owned(),
                    value: self.wire_profile.client_mode(),
                },
                DashboardWireAttribute {
                    label: "Token 认证".to_owned(),
                    value: "xai-grok-cli".to_owned(),
                },
            ],
            verified_at: Some(self.wire_profile.verified_at()),
            release: None,
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
        let Some(breakdown) = grok_billing_breakdown(
            &input.upstream_model_id,
            input_tokens,
            output_tokens,
            input.cached_tokens.unwrap_or_default(),
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
        let document = serde_json::to_vec(&Value::Object(
            command.document.into_provider_data().into_inner(),
        ))
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
        let document = GrokOAuthImportDocument::parse_json(&document)
            .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
        let discovery = self.oauth.discover().await.map_err(map_oauth_error)?;
        let policy = self
            .runtime_policy
            .load_refresh_policy()
            .await
            .map_err(map_provider_store_error)?;
        let mut subjects = BTreeSet::new();
        let mut credentials = Vec::new();
        for entry in document.into_entries() {
            let name = entry.name().to_owned();
            let email = entry.email().map(str::to_owned);
            let tokens = self
                .oauth
                .verify_imported_credential(&discovery, entry.into_candidate())
                .await
                .map_err(|error| map_failure_class(error.class()))?;
            if !subjects.insert(tokens.evidence().subject().to_owned()) {
                return Err(provider_error(ProviderAdminErrorKind::Invalid));
            }
            let prepared = GrokCredentialAdmin
                .prepare_verified_account(
                    &VerifiedGrokAccount {
                        account_id: ProviderAccountId::new(format!(
                            "acct_{}",
                            Uuid::now_v7().simple()
                        ))
                        .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?,
                        provider_instance_id: command.provider_instance_id.clone(),
                        name,
                        email,
                        upstream_account_id: None,
                        plan_type: None,
                        tokens,
                        enabled: true,
                    },
                    policy,
                )
                .map_err(map_repository_error)?;
            credentials.push(prepared_create(prepared, Utc::now())?);
        }
        Ok(PreparedCredentialImport {
            provider_kind: self.provider_kind.clone(),
            provider_instance_id: command.provider_instance_id,
            credentials,
        })
    }

    async fn start_authorization(
        &self,
        pending: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        if pending.provider_kind() != &self.provider_kind {
            return Err(provider_error(ProviderAdminErrorKind::Invalid));
        }
        if let AuthorizationMutationTarget::Reauthorize {
            provider_instance_id,
            account_id,
            expected_credential_revision,
        } = pending.target()
        {
            let revision = CredentialRevision::new(expected_credential_revision.get())
                .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
            let current = self
                .accounts
                .load_credential(account_id, revision)
                .await
                .map_err(map_store_error)?;
            if current.account.provider() != &self.provider_kind
                || current.account.instance() != provider_instance_id
            {
                return Err(provider_error(ProviderAdminErrorKind::NotFound));
            }
        }
        let discovery = self.oauth.discover().await.map_err(map_oauth_error)?;
        let redirect = RedirectUriAllowlist::new([crate::OFFICIAL_REDIRECT_URI])
            .and_then(|allowlist| allowlist.authorize(crate::OFFICIAL_REDIRECT_URI))
            .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?;
        let authorization = self
            .oauth
            .start_authorization_code(&discovery, redirect, None)
            .map_err(map_oauth_error)?;
        self.store_pending(pending, authorization).await
    }

    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        let stored = self
            .take_pending(&command.context, &command.flow_id)
            .await?;
        let authorization = PendingAuthorization::from_server_state(
            &self.oauth_config,
            &SecretValue::new(stored.server_state),
        )
        .map_err(map_oauth_error)?;
        let callback = callback(&command.callback_url)?;
        let grant = authorization
            .accept_callback(callback)
            .map_err(map_oauth_error)?;
        let discovery = self.oauth.discover().await.map_err(map_oauth_error)?;
        let tokens = self
            .oauth
            .exchange_authorization_code(&discovery, grant)
            .await
            .map_err(map_oauth_error)?;
        let policy = self
            .runtime_policy
            .load_refresh_policy()
            .await
            .map_err(map_provider_store_error)?;
        let credential = match stored.mutation.target() {
            AuthorizationMutationTarget::Create {
                provider_instance_id,
                name,
            } => {
                let prepared = GrokCredentialAdmin
                    .prepare_verified_account(
                        &VerifiedGrokAccount {
                            account_id: ProviderAccountId::new(format!(
                                "acct_{}",
                                Uuid::now_v7().simple()
                            ))
                            .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?,
                            provider_instance_id: provider_instance_id.clone(),
                            name: name.clone(),
                            email: None,
                            upstream_account_id: None,
                            plan_type: None,
                            tokens,
                            enabled: true,
                        },
                        policy,
                    )
                    .map_err(map_repository_error)?;
                PreparedAuthorizationCredential::Create(prepared_create(prepared, Utc::now())?)
            }
            AuthorizationMutationTarget::Reauthorize {
                provider_instance_id,
                account_id,
                expected_credential_revision,
            } => {
                let revision = CredentialRevision::new(expected_credential_revision.get())
                    .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?;
                let current = self
                    .accounts
                    .load_credential(account_id, revision)
                    .await
                    .map_err(map_store_error)?;
                if current.account.provider() != &self.provider_kind
                    || current.account.instance() != provider_instance_id
                {
                    return Err(provider_error(ProviderAdminErrorKind::NotFound));
                }
                let prepared = verified_rotation(current, tokens, policy)?;
                PreparedAuthorizationCredential::Reauthorize(prepared_rotation(
                    prepared,
                    provider_instance_id.clone(),
                    self.provider_kind.clone(),
                )?)
            }
        };
        Ok(PreparedAuthorizationCommit {
            pending: stored.mutation,
            credential,
        })
    }

    async fn prepare_rotation(
        &self,
        command: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        validate_account_record(&command.account, &self.provider_kind)?;
        if command.account.credential_revision != command.expected_credential_revision {
            return Err(provider_error(ProviderAdminErrorKind::Conflict));
        }
        let account_id = ProviderAccountId::new(command.account.id.clone())
            .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
        let revision = CredentialRevision::new(command.expected_credential_revision.get())
            .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
        let current = self
            .accounts
            .load_credential(&account_id, revision)
            .await
            .map_err(map_store_error)?;
        if !account_matches_record(&current.account, &command.account) {
            return Err(provider_error(ProviderAdminErrorKind::Conflict));
        }
        let rotation: RotationDocument = serde_json::from_value(Value::Object(
            command.provider_material.into_provider_data().into_inner(),
        ))
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
        let tokens = match rotation.id_token {
            Some(id_token) => GrokOAuthImportTokens::new(
                SecretValue::new(rotation.access_token),
                SecretValue::new(rotation.refresh_token),
                SecretValue::new(id_token),
            ),
            None => GrokOAuthImportTokens::without_id_token(
                SecretValue::new(rotation.access_token),
                SecretValue::new(rotation.refresh_token),
            ),
        };
        let candidate = GrokOAuthImportCandidate::new(
            tokens,
            GrokOAuthImportMetadata::new(
                "Bearer".to_owned(),
                crate::OFFICIAL_CLIENT_ID.to_owned(),
                rotation.scope,
                GROK_CLI_BASE_URL.to_owned(),
                Utc::now(),
                rotation.expires_at,
            ),
        );
        let discovery = self.oauth.discover().await.map_err(map_oauth_error)?;
        let tokens = self
            .oauth
            .verify_imported_credential(&discovery, candidate)
            .await
            .map_err(|error| map_failure_class(error.class()))?;
        let policy = self
            .runtime_policy
            .load_refresh_policy()
            .await
            .map_err(map_provider_store_error)?;
        let prepared = verified_rotation(current, tokens, policy)?;
        prepared_rotation(
            prepared,
            command.account.provider_instance_id,
            command.account.provider_kind,
        )
    }

    async fn prepare_refresh(
        &self,
        command: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        validate_account_record(&command.account, &self.provider_kind)?;
        let account_id = ProviderAccountId::new(command.account.id.clone())
            .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
        let current = self.account(&account_id).await?;
        if !account_matches_record(&current, &command.account) {
            return Err(provider_error(ProviderAdminErrorKind::Conflict));
        }
        let revision = CredentialRevision::new(command.account.credential_revision.get())
            .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
        let prepared = self
            .refresh
            .prepare_manual_refresh(&account_id, revision)
            .await
            .map_err(map_refresh_error)?;
        prepared_rotation(
            prepared,
            command.account.provider_instance_id,
            command.account.provider_kind,
        )
    }

    async fn quota(
        &self,
        request: ProviderQuotaRequest,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        let ProviderQuotaRequest {
            account_id,
            refresh,
            rolling_usage,
        } = request;
        self.account(&account_id).await?;
        let lifecycle = self
            .repository
            .read_lifecycle(&account_id)
            .await
            .map_err(map_repository_error)?;
        let snapshot = if refresh {
            Some(
                self.quota
                    .refresh_account(&account_id)
                    .await
                    .map_err(map_quota_error)?,
            )
        } else {
            self.quota
                .read_account(&account_id)
                .await
                .map_err(map_quota_error)?
        };
        Ok(project_quota(
            snapshot,
            lifecycle.refresh_token_expires_at().copied(),
            rolling_usage,
        ))
    }

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        let account = self.account(account_id).await?;
        let catalog = if refresh {
            Some(
                self.catalog
                    .refresh_account_catalog(account_id)
                    .await
                    .map_err(map_catalog_error)?,
            )
        } else {
            self.catalog
                .read_account_catalog(account_id, account.revision())
                .await
                .map_err(map_catalog_error)?
        };
        let Some(catalog) = catalog else {
            return Ok(ProviderModels {
                models: Vec::new(),
                observed_at: None,
            });
        };
        let models = catalog
            .seed()
            .models()
            .iter()
            .map(|model| {
                Ok(ProviderModel {
                    id: UpstreamModelId::new(model.clone())
                        .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?,
                    name: model.clone(),
                })
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
        let mut loaded = Vec::with_capacity(credentials.len());
        for input in credentials {
            validate_account_record(&input.account, &self.provider_kind)?;
            let current = LoadedCredential {
                account: account_from_record(&input.account)?,
                credential: PlaintextCredential::new(
                    input.provider_material.into_provider_data().into_inner(),
                ),
            };
            account_ids.push(current.account.id().clone());
            loaded.push(current);
        }
        let document = GrokCredentialAdmin
            .export_oauth_bundle(&loaded, Utc::now())
            .map_err(map_repository_error)?
            .into_value();
        let Value::Object(document) = document else {
            return Err(provider_error(ProviderAdminErrorKind::Internal));
        };
        Ok(ProviderExport {
            provider_kind: self.provider_kind.clone(),
            account_ids,
            document: ProviderDocument::new(OpaqueProviderData::new(document)),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RotationDocument {
    access_token: String,
    refresh_token: String,
    id_token: Option<String>,
    scope: String,
    expires_at: DateTime<Utc>,
}

struct XaiCredentialCommitGuard {
    _guard: PreparedGrokCredentialRotationGuard,
}

impl CredentialCommitGuard for XaiCredentialCommitGuard {
    fn finish(self: Box<Self>) {}
}

fn verified_rotation(
    current: LoadedCredential,
    tokens: VerifiedTokenSet,
    policy: ProviderRefreshPolicy,
) -> Result<PreparedGrokCredentialRotation, ProviderAdminError> {
    let expires_in = tokens
        .expires_in()
        .ok_or_else(|| provider_error(ProviderAdminErrorKind::Invalid))?;
    let refresh_token = tokens
        .refresh_token()
        .cloned()
        .ok_or_else(|| provider_error(ProviderAdminErrorKind::Invalid))?;
    let now = SystemTime::now();
    let access_token_expires_at = now
        .checked_add(expires_in)
        .ok_or_else(|| provider_error(ProviderAdminErrorKind::Invalid))?;
    let next_refresh_at = policy
        .next_attempt_at(current.account.id(), access_token_expires_at, now)
        .map_err(map_provider_store_error)?;
    GrokCredentialAdmin
        .prepare_rotation(&RotateManagedGrokCredential {
            secret: GrokOAuthSecret {
                access_token: tokens.access_token().clone(),
                refresh_token,
                id_token: tokens.id_token().cloned(),
                scope: tokens.scope().to_owned(),
            },
            verified_account: GrokAccountProfile {
                subject: tokens.evidence().subject().to_owned(),
                email: current.account.email().map(str::to_owned),
                upstream_account_id: current.account.upstream_account_id().map(str::to_owned),
                plan_type: current.account.plan_type().map(str::to_owned),
                access_token_expires_at: access_token_expires_at.into(),
                refresh_token_expires_at: None,
            },
            current,
            next_refresh_at: next_refresh_at.into(),
        })
        .map_err(map_repository_error)
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
        provider_instance_id: account.instance().clone(),
        provider_kind: account.provider().clone(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().to_owned(),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        provider_material: ProviderDocument::new(OpaqueProviderData::new(credential.into_inner())),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: account.access_token_expires_at().into(),
        next_refresh_at: account.next_refresh_at().map(Into::into),
        enabled: account.enabled(),
        availability: admin_availability(account.availability()),
        availability_reason: None,
        cooldown_until: account.cooldown_until().map(Into::into),
        availability_observed_at: observed_at,
    })
}

fn prepared_rotation(
    prepared: PreparedGrokCredentialRotation,
    provider_instance_id: ProviderInstanceId,
    provider_kind: ProviderKind,
) -> Result<PreparedCredentialRotation, ProviderAdminError> {
    let (profile, credential, guard) = prepared.into_parts();
    let (
        account_id,
        expected_revision,
        credential_profile,
        credential,
        has_refresh_token,
        access_token_expires_at,
        next_refresh_at,
    ) = credential.into_parts();
    if profile != credential_profile || profile.account_id != account_id {
        return Err(provider_error(ProviderAdminErrorKind::Internal));
    }
    Ok(PreparedCredentialRotation::new(
        PreparedCredentialRotationFacts {
            account_id,
            provider_instance_id,
            provider_kind,
            expected_credential_revision: Revision::new(expected_revision.get())
                .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?,
            name: profile.name,
            email: profile.email,
            plan_type: profile.plan_type,
            provider_material: ProviderDocument::new(OpaqueProviderData::new(
                credential.into_inner(),
            )),
            has_refresh_token,
            access_token_expires_at: access_token_expires_at.into(),
            next_refresh_at: next_refresh_at.map(Into::into),
        },
        Box::new(XaiCredentialCommitGuard { _guard: guard }),
    ))
}

fn validate_account_record(
    account: &AccountRecord,
    provider_kind: &ProviderKind,
) -> Result<(), ProviderAdminError> {
    if &account.provider_kind != provider_kind || provider_kind.as_str() != XAI_PROVIDER_NAME {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    Ok(())
}

fn account_from_record(account: &AccountRecord) -> Result<ProviderAccount, ProviderAdminError> {
    let account_id = ProviderAccountId::new(account.id.clone())
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    let revision = CredentialRevision::new(account.credential_revision.get())
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    Ok(ProviderAccount::new(
        account_id,
        account.provider_instance_id.clone(),
        account.provider_kind.clone(),
        account.name.clone(),
        account.upstream_user_id.clone(),
        revision,
        account.access_token_expires_at.into(),
    )
    .with_profile(
        account.email.clone(),
        account.upstream_account_id.clone(),
        account.plan_type.clone(),
    )
    .with_runtime_state(
        account.enabled,
        core_availability(account.availability),
        account.cooldown_until.map(Into::into),
    )
    .with_refresh_schedule(
        account.has_refresh_token,
        account.next_refresh_at.map(Into::into),
    ))
}

fn account_matches_record(account: &ProviderAccount, record: &AccountRecord) -> bool {
    account.id().as_str() == record.id
        && account.instance() == &record.provider_instance_id
        && account.provider() == &record.provider_kind
        && account.revision().get() == record.credential_revision.get()
        && account.name() == record.name
        && account.email() == record.email.as_deref()
        && account.upstream_user_id() == record.upstream_user_id
        && account.upstream_account_id() == record.upstream_account_id.as_deref()
        && account.plan_type() == record.plan_type.as_deref()
        && account.enabled() == record.enabled
        && admin_availability(account.availability()) == record.availability
        && account.cooldown_until().map(DateTime::<Utc>::from) == record.cooldown_until
        && DateTime::<Utc>::from(account.access_token_expires_at())
            == record.access_token_expires_at
        && account.next_refresh_at().map(DateTime::<Utc>::from) == record.next_refresh_at
        && account.has_refresh_token() == record.has_refresh_token
}

const fn admin_availability(value: AccountAvailability) -> AdminAccountAvailability {
    match value {
        AccountAvailability::Unknown => AdminAccountAvailability::Unknown,
        AccountAvailability::Ready => AdminAccountAvailability::Ready,
        AccountAvailability::Cooldown => AdminAccountAvailability::Cooldown,
        AccountAvailability::QuotaExhausted => AdminAccountAvailability::QuotaExhausted,
        AccountAvailability::Expired => AdminAccountAvailability::Expired,
        AccountAvailability::Banned => AdminAccountAvailability::Banned,
        AccountAvailability::Invalid => AdminAccountAvailability::Invalid,
    }
}

const fn core_availability(value: AdminAccountAvailability) -> AccountAvailability {
    match value {
        AdminAccountAvailability::Unknown => AccountAvailability::Unknown,
        AdminAccountAvailability::Ready => AccountAvailability::Ready,
        AdminAccountAvailability::Cooldown => AccountAvailability::Cooldown,
        AdminAccountAvailability::QuotaExhausted => AccountAvailability::QuotaExhausted,
        AdminAccountAvailability::Expired => AccountAvailability::Expired,
        AdminAccountAvailability::Banned => AccountAvailability::Banned,
        AdminAccountAvailability::Invalid => AccountAvailability::Invalid,
    }
}

fn project_quota(
    snapshot: Option<GrokQuotaSnapshot>,
    refresh_token_expires_at: Option<DateTime<Utc>>,
    rolling_usage: Option<gateway_admin::model::accounts::AccountUsage>,
) -> ProviderQuota {
    let Some(snapshot) = snapshot else {
        return ProviderQuota {
            observed_at: None,
            refresh_token_expires_at,
            windows: Vec::new(),
            provider_data: None,
        };
    };
    let billing = snapshot.billing();
    let window = if billing.has_authoritative_quota() {
        let period_kind = billing.period_kind();
        let window_seconds = quota_window_seconds(billing.period_start(), billing.period_end());
        let mut data = Map::new();
        data.insert(
            "periodStart".to_owned(),
            billing
                .period_start()
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
        );
        data.insert(
            "periodEnd".to_owned(),
            billing
                .period_end()
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
        );
        for (key, value) in [
            ("monthlyLimitCents", billing.monthly_limit_cents()),
            ("includedUsedCents", billing.included_used_cents()),
            ("onDemandCapCents", billing.on_demand_cap_cents()),
            ("onDemandUsedCents", billing.on_demand_used_cents()),
            ("prepaidBalanceCents", billing.prepaid_balance_cents()),
        ] {
            data.insert(
                key.to_owned(),
                value.map_or(Value::Null, |value| Value::Number(Number::from(value))),
            );
        }
        ProviderQuotaWindow {
            key: billing.period_type().unwrap_or("billing").to_owned(),
            group: quota_group(period_kind).to_owned(),
            label: xai_quota_window_label(period_kind, window_seconds),
            source: None,
            window_seconds,
            used_percent: billing.used_percent(),
            reset_at: billing.period_end().and_then(parse_utc),
            local_usage: None,
            provider_data: Some(ProviderDocument::new(OpaqueProviderData::new(data))),
        }
    } else {
        ProviderQuotaWindow {
            key: "free-rolling-24h".to_owned(),
            group: "shortTerm".to_owned(),
            label: "天限额".to_owned(),
            source: None,
            window_seconds: Some(crate::GROK_FREE_ROLLING_WINDOW_SECONDS),
            used_percent: None,
            reset_at: None,
            local_usage: rolling_usage,
            provider_data: None,
        }
    };
    ProviderQuota {
        observed_at: Some(snapshot.observed_at()),
        refresh_token_expires_at,
        windows: vec![window],
        provider_data: None,
    }
}

const fn quota_group(kind: GrokQuotaPeriodKind) -> &'static str {
    match kind {
        GrokQuotaPeriodKind::Weekly => "shortTerm",
        GrokQuotaPeriodKind::Monthly => "monthly",
        GrokQuotaPeriodKind::Other => "other",
    }
}

fn xai_quota_window_label(kind: GrokQuotaPeriodKind, window_seconds: Option<u64>) -> String {
    match kind {
        GrokQuotaPeriodKind::Weekly => "周限额".to_owned(),
        GrokQuotaPeriodKind::Monthly => "月限额".to_owned(),
        GrokQuotaPeriodKind::Other => custom_quota_window_label(window_seconds),
    }
}

fn custom_quota_window_label(window_seconds: Option<u64>) -> String {
    let Some(seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        return "额度".to_owned();
    };
    if seconds % 86_400 == 0 {
        format!("{}天限额", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}小时限额", seconds / 3_600)
    } else {
        format!("{}分钟限额", seconds.div_ceil(60))
    }
}

fn quota_window_seconds(start: Option<&str>, end: Option<&str>) -> Option<u64> {
    let start = start.and_then(parse_utc)?;
    let end = end.and_then(parse_utc)?;
    end.signed_duration_since(start)
        .num_seconds()
        .try_into()
        .ok()
}

fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

struct StoredXaiAuthorization {
    flow_id: String,
    owner_ref: String,
    expires_at: DateTime<Utc>,
    server_state: String,
    mutation: PendingAuthorizationMutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingDocument {
    schema_version: u64,
    flow_id: String,
    owner_ref: String,
    expires_at: DateTime<Utc>,
    server_state: String,
    mutation: PendingMutationDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingMutationDocument {
    expected_config_revision: u64,
    provider_kind: String,
    target: PendingTargetDocument,
    owner: PendingOwnerDocument,
    started_request_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PendingTargetDocument {
    Create {
        provider_instance_id: String,
        name: String,
    },
    Reauthorize {
        provider_instance_id: String,
        account_id: String,
        expected_credential_revision: u64,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PendingOwnerDocument {
    AdminSession { admin_user_id: String },
    AdminApiKey,
    System,
}

fn encode_pending(pending: &StoredXaiAuthorization) -> Map<String, Value> {
    let mut document = Map::new();
    document.insert(
        "schema_version".to_owned(),
        Value::Number(Number::from(PENDING_SCHEMA_VERSION)),
    );
    document.insert("flow_id".to_owned(), Value::String(pending.flow_id.clone()));
    document.insert(
        "owner_ref".to_owned(),
        Value::String(pending.owner_ref.clone()),
    );
    document.insert(
        "expires_at".to_owned(),
        Value::String(pending.expires_at.to_rfc3339()),
    );
    document.insert(
        "server_state".to_owned(),
        Value::String(pending.server_state.clone()),
    );
    document.insert(
        "mutation".to_owned(),
        Value::Object(encode_mutation(&pending.mutation)),
    );
    document
}

fn encode_mutation(mutation: &PendingAuthorizationMutation) -> Map<String, Value> {
    let mut document = Map::new();
    document.insert(
        "expected_config_revision".to_owned(),
        Value::Number(Number::from(mutation.expected_config_revision().get())),
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
        AuthorizationMutationTarget::Create {
            provider_instance_id,
            name,
        } => {
            document.insert("kind".to_owned(), Value::String("create".to_owned()));
            document.insert(
                "provider_instance_id".to_owned(),
                Value::String(provider_instance_id.to_string()),
            );
            document.insert("name".to_owned(), Value::String(name.clone()));
        }
        AuthorizationMutationTarget::Reauthorize {
            provider_instance_id,
            account_id,
            expected_credential_revision,
        } => {
            document.insert("kind".to_owned(), Value::String("reauthorize".to_owned()));
            document.insert(
                "provider_instance_id".to_owned(),
                Value::String(provider_instance_id.to_string()),
            );
            document.insert(
                "account_id".to_owned(),
                Value::String(account_id.to_string()),
            );
            document.insert(
                "expected_credential_revision".to_owned(),
                Value::Number(Number::from(expected_credential_revision.get())),
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
    oauth_config: &GrokOAuthConfig,
) -> Result<StoredXaiAuthorization, ProviderAdminError> {
    let document: PendingDocument = serde_json::from_value(Value::Object(payload.into_inner()))
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    if document.schema_version != PENDING_SCHEMA_VERSION
        || !valid_pending_text(&document.flow_id)
        || !valid_pending_text(&document.owner_ref)
        || document.expires_at <= Utc::now()
    {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    let mutation = decode_mutation(document.mutation)?;
    if mutation.provider_kind().as_str() != XAI_PROVIDER_NAME
        || owner_ref(mutation.owner_binding().owner()) != document.owner_ref
    {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    PendingAuthorization::from_server_state(
        oauth_config,
        &SecretValue::new(document.server_state.clone()),
    )
    .map_err(map_oauth_error)?;
    Ok(StoredXaiAuthorization {
        flow_id: document.flow_id,
        owner_ref: document.owner_ref,
        expires_at: document.expires_at,
        server_state: document.server_state,
        mutation,
    })
}

fn decode_mutation(
    document: PendingMutationDocument,
) -> Result<PendingAuthorizationMutation, ProviderAdminError> {
    let provider_kind = ProviderKind::new(document.provider_kind)
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    let target = match document.target {
        PendingTargetDocument::Create {
            provider_instance_id,
            name,
        } => AuthorizationMutationTarget::Create {
            provider_instance_id: ProviderInstanceId::new(provider_instance_id)
                .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?,
            name,
        },
        PendingTargetDocument::Reauthorize {
            provider_instance_id,
            account_id,
            expected_credential_revision,
        } => AuthorizationMutationTarget::Reauthorize {
            provider_instance_id: ProviderInstanceId::new(provider_instance_id)
                .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?,
            account_id: ProviderAccountId::new(account_id)
                .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?,
            expected_credential_revision: Revision::new(expected_credential_revision)
                .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?,
        },
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
        Revision::new(document.expected_config_revision)
            .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?,
        provider_kind,
        target,
        AuthorizationOwnerBinding::from_context(&context),
    ))
}

fn callback(value: &str) -> Result<AuthorizationCallback, ProviderAdminError> {
    let mut callback =
        Url::parse(value).map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    if callback.fragment().is_some() {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    let query = callback.query().unwrap_or_default().to_owned();
    callback.set_query(None);
    let expected = Url::parse(crate::OFFICIAL_REDIRECT_URI)
        .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?;
    if callback != expected {
        return Err(provider_error(ProviderAdminErrorKind::Invalid));
    }
    AuthorizationCallback::parse(&query)
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))
}

fn random_flow_id() -> Result<String, ProviderAdminError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| provider_error(ProviderAdminErrorKind::Unavailable))?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

fn owner_ref(owner: &AuthorizationOwner) -> String {
    let mut digest = Sha256::new();
    match owner {
        AuthorizationOwner::AdminSession { admin_user_id } => {
            digest.update(b"admin-session\0");
            digest.update(admin_user_id.as_bytes());
        }
        AuthorizationOwner::AdminApiKey => digest.update(b"admin-api-key"),
        AuthorizationOwner::System => digest.update(b"system"),
    }
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

fn binding(value: &str) -> Result<OAuthPendingBinding, ProviderAdminError> {
    OAuthPendingBinding::try_new(value.to_owned()).map_err(map_provider_store_error)
}

fn valid_pending_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PENDING_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

fn currency_cost(money: Money) -> Result<CurrencyCost, ProviderAdminError> {
    Ok(CurrencyCost {
        currency: money.currency().as_str().to_owned(),
        amount: money
            .amount()
            .to_string()
            .parse::<DecimalAmount>()
            .map_err(|_| provider_error(ProviderAdminErrorKind::Internal))?,
    })
}

fn provider_error(kind: ProviderAdminErrorKind) -> ProviderAdminError {
    ProviderAdminError::new(kind)
}

fn build_connection_test_operation(
    upstream_model: &UpstreamModelId,
    input_text: &str,
) -> Result<Operation, ProviderAdminError> {
    let message = Message::new(
        MessageRole::User,
        vec![ContentPart::Text(input_text.to_owned())],
    )
    .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
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
        .map_err(|_| provider_error(ProviderAdminErrorKind::Invalid))?;
    Ok(Operation::Generate(
        GenerateRequest::from_protocol_payload(vec![message], payload)
            .with_response_persistence(ResponsePersistence::DoNotStore),
    ))
}

fn map_failure_class(class: FailureClass) -> ProviderAdminError {
    provider_error(match class {
        FailureClass::Transient => ProviderAdminErrorKind::Unavailable,
        FailureClass::Ambiguous => ProviderAdminErrorKind::Conflict,
        FailureClass::CredentialPermanent
        | FailureClass::ConfigurationPermanent
        | FailureClass::UserActionRequired
        | FailureClass::Security => ProviderAdminErrorKind::Invalid,
        FailureClass::Unsupported => ProviderAdminErrorKind::Unsupported,
    })
}

fn map_oauth_error(error: OAuthError) -> ProviderAdminError {
    map_failure_class(error.class())
}

fn map_provider_store_error(error: ProviderStoreError) -> ProviderAdminError {
    provider_error(match error.kind() {
        ProviderStoreErrorKind::InvalidData => ProviderAdminErrorKind::Invalid,
        ProviderStoreErrorKind::Conflict => ProviderAdminErrorKind::Conflict,
        ProviderStoreErrorKind::Unavailable => ProviderAdminErrorKind::Unavailable,
    })
}

fn map_store_error(error: gateway_core::error::StoreError) -> ProviderAdminError {
    provider_error(match error.kind() {
        StoreErrorKind::Conflict => ProviderAdminErrorKind::Conflict,
        StoreErrorKind::InvalidData | StoreErrorKind::InvalidState => {
            ProviderAdminErrorKind::NotFound
        }
        StoreErrorKind::Unavailable => ProviderAdminErrorKind::Unavailable,
        _ => ProviderAdminErrorKind::Internal,
    })
}

fn map_repository_error(error: GrokCredentialRepositoryError) -> ProviderAdminError {
    use GrokCredentialRepositoryError as Error;
    provider_error(match error {
        Error::InvalidInput(_)
        | Error::WrongProviderKind
        | Error::IdentityRebind
        | Error::InvalidCredentialData => ProviderAdminErrorKind::Invalid,
        Error::CredentialNotFound => ProviderAdminErrorKind::NotFound,
        Error::StaleCredentialRevision | Error::Conflict | Error::RevisionOverflow => {
            ProviderAdminErrorKind::Conflict
        }
        Error::Store => ProviderAdminErrorKind::Unavailable,
    })
}

fn map_refresh_error(error: GrokCredentialRefreshError) -> ProviderAdminError {
    use GrokCredentialRefreshError as Error;
    match error {
        Error::Repository(error) => map_repository_error(error),
        Error::Lease(error) => map_provider_store_error(error),
        Error::LeaseBusy => provider_error(ProviderAdminErrorKind::Conflict),
        Error::InvalidRefreshResponse => provider_error(ProviderAdminErrorKind::Invalid),
        Error::Preparation => provider_error(ProviderAdminErrorKind::Unavailable),
        Error::ManualFailure(failure) => map_failure_class(match failure {
            crate::GrokRefreshFailure::Transient => FailureClass::Transient,
            crate::GrokRefreshFailure::Ambiguous => FailureClass::Ambiguous,
            crate::GrokRefreshFailure::InvalidGrant
            | crate::GrokRefreshFailure::Banned
            | crate::GrokRefreshFailure::Rejected => FailureClass::CredentialPermanent,
        }),
    }
}

fn map_quota_error(error: GrokQuotaError) -> ProviderAdminError {
    use GrokQuotaError as Error;
    provider_error(match error {
        Error::AccountUnavailable => ProviderAdminErrorKind::NotFound,
        Error::StaleCredentialSnapshot => ProviderAdminErrorKind::Conflict,
        Error::InvalidData => ProviderAdminErrorKind::Invalid,
        Error::Upstream | Error::Store => ProviderAdminErrorKind::Unavailable,
    })
}

fn map_catalog_error(error: GrokCredentialCatalogError) -> ProviderAdminError {
    use GrokCredentialCatalogError as Error;
    provider_error(match error {
        Error::InvalidInstance | Error::InvalidCredentialData | Error::ConflictingModelFacts => {
            ProviderAdminErrorKind::Invalid
        }
        Error::NoEligibleCredential => ProviderAdminErrorKind::NotFound,
        Error::StaleCredentialSnapshot => ProviderAdminErrorKind::Conflict,
        Error::Upstream | Error::Cache | Error::Store => ProviderAdminErrorKind::Unavailable,
    })
}
