//! 复用网关刷新策略、账号租约及凭据版本提交后台续期

use super::{oauth::error, service::CredentialService};
use gateway_admin::ports::provider::{ProviderAdminError, ProviderAdminErrorKind as Kind};
use gateway_core::{
    account::{
        CredentialCasOutcome, CredentialCasUpdate, CredentialRevision, CredentialState,
        LoadedCredential, PlaintextCredential, ProviderAccountUpdate, ProviderRefreshQuery,
    },
    provider_ports::provider_refresh_backoff_at,
    task::WorkerTaskError,
};
use std::{
    num::NonZeroU32,
    time::{Duration, SystemTime},
};

fn update(
    loaded: &LoadedCredential,
    material: PlaintextCredential,
    expires: Option<SystemTime>,
    next: Option<SystemTime>,
) -> Result<CredentialCasUpdate, ProviderAdminError> {
    let a = &loaded.account;
    let profile = ProviderAccountUpdate {
        account_id: a.id().clone(),
        name: a.name().into(),
        email: a.email().map(str::to_owned),
        plan_type: a.plan_type().map(str::to_owned),
    };
    CredentialCasUpdate::new(
        a.id().clone(),
        a.revision(),
        profile,
        material,
        true,
        expires,
        next,
    )
    .map(CredentialCasUpdate::preserving_profile)
    .map_err(|_| error(Kind::Internal))
}

impl CredentialService {
    pub(crate) async fn refresh_candidates(
        &self,
    ) -> Result<(Vec<LoadedCredential>, usize), WorkerTaskError> {
        let policy = self
            .runtime_policy
            .load_refresh_policy()
            .await
            .map_err(|e| WorkerTaskError::safe("无法读取 OAuth 刷新策略").with_source(e))?;
        let now = SystemTime::now();
        let query = ProviderRefreshQuery::new(
            self.kind.clone(),
            now.checked_add(policy.margin()).unwrap_or(now),
            SystemTime::UNIX_EPOCH,
            now,
            Vec::new(),
            NonZeroU32::new(64).unwrap_or(NonZeroU32::MIN),
        );
        let candidates = self
            .accounts
            .list_refresh_candidates(query)
            .await
            .map_err(|e| WorkerTaskError::safe("无法读取到期 OAuth 账号").with_source(e))?;
        Ok((candidates, policy.concurrency().get() as usize))
    }

    pub(crate) async fn refresh_due(
        &self,
        loaded: &LoadedCredential,
    ) -> Result<CredentialRevision, ProviderAdminError> {
        let (facts, guard) = self.refresh_current(loaded).await?.into_parts();
        let material =
            PlaintextCredential::new(facts.provider_material.into_provider_data().into_inner());
        let record = update(
            loaded,
            material,
            facts.access_token_expires_at.map(Into::into),
            None,
        )?
        .with_account_state(CredentialState::Ready, SystemTime::now(), None, None);
        match self
            .accounts
            .compare_and_swap_credential(record)
            .await
            .map_err(|e| error(Kind::Unavailable).with_source(e))?
        {
            CredentialCasOutcome::Updated(revision) => {
                guard.finish();
                if self
                    .credential_state
                    .clear_refresh_backoff(loaded.account.id())
                    .await
                    .is_err()
                {
                    // 凭据已经提交，清理可丢失退避失败不能丢掉新 revision 的目录续接
                    tracing::warn!("Antigravity 续期后清理退避状态失败");
                }
                Ok(revision)
            }
            CredentialCasOutcome::Conflict => Err(error(Kind::Conflict)),
        }
    }

    pub(super) async fn defer_refresh(
        &self,
        loaded: &LoadedCredential,
    ) -> Result<(), ProviderAdminError> {
        let id = loaded.account.id();
        let attempt = self
            .credential_state
            .record_refresh_backoff(id, Duration::from_secs(86400))
            .await
            .map_err(|e| error(Kind::Unavailable).with_source(e))?;
        let retry =
            provider_refresh_backoff_at(id, SystemTime::now(), attempt, "antigravity_oauth")
                .map_err(|e| error(Kind::Unavailable).with_source(e))?;
        let record = update(
            loaded,
            loaded.credential.clone(),
            loaded.account.access_token_expires_at(),
            Some(retry),
        )?
        .preserving_access_token_expiry();
        match self
            .accounts
            .compare_and_swap_credential(record)
            .await
            .map_err(|e| error(Kind::Unavailable).with_source(e))?
        {
            CredentialCasOutcome::Updated(_) => Ok(()),
            CredentialCasOutcome::Conflict => Err(error(Kind::Conflict)),
        }
    }
}
