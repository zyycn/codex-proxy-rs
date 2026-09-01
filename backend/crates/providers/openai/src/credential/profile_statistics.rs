//! OpenAI 当前账号个人资料统计与官方头像查询编排。

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use gateway_core::account::ProviderAccountId;
use reqwest::Client;
use secrecy::ExposeSecret as _;
use thiserror::Error;
use uuid::Uuid;

use crate::transport::profile::CodexWireProfileState;
use crate::transport::{
    CodexBackendClient, CodexClientError, CodexProfileAvatar, CodexProfileAvatarFetchError,
    CodexProfileStatistics, CodexRequestContext, fetch_profile_avatar,
};

use super::CodexCredentialRepository;

const PROFILE_AVATAR_SOURCE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Error)]
pub enum CodexProfileStatisticsError {
    #[error("Codex profile-statistics credential data is invalid")]
    InvalidCredentialData,
    #[error("Codex OAuth access token must be refreshed before querying profile statistics")]
    CredentialRefreshRequired { upstream_body: Option<String> },
    #[error("Codex profile-statistics account was not found")]
    NotFound,
    #[error("provider account store is unavailable: {detail}")]
    Store { detail: String },
    #[error("Codex profile-statistics upstream returned HTTP {status}")]
    Upstream {
        status: u16,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("Codex profile-statistics query transport is unavailable")]
    TransportUnavailable,
}

impl std::fmt::Debug for CodexProfileStatisticsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCredentialData => formatter.write_str("InvalidCredentialData"),
            Self::CredentialRefreshRequired { .. } => {
                formatter.write_str("CredentialRefreshRequired { upstream_body: <redacted> }")
            }
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Store { .. } => formatter.write_str("Store { detail: <redacted> }"),
            Self::Upstream {
                status,
                retry_after_seconds,
                ..
            } => formatter
                .debug_struct("Upstream")
                .field("status", status)
                .field("body", &"<redacted>")
                .field("retry_after_seconds", retry_after_seconds)
                .finish(),
            Self::TransportUnavailable => formatter.write_str("TransportUnavailable"),
        }
    }
}

#[derive(Error)]
pub enum CodexProfileAvatarError {
    #[error(transparent)]
    ProfileStatistics(#[from] CodexProfileStatisticsError),
    #[error("Codex profile does not provide an avatar")]
    Missing,
    #[error("Codex profile avatar source is invalid")]
    InvalidSource,
    #[error("Codex profile avatar upstream returned HTTP {status}")]
    Upstream { status: u16 },
    #[error("Codex profile avatar transport is unavailable")]
    TransportUnavailable,
}

impl std::fmt::Debug for CodexProfileAvatarError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProfileStatistics(error) => formatter
                .debug_tuple("ProfileStatistics")
                .field(error)
                .finish(),
            Self::Missing => formatter.write_str("Missing"),
            Self::InvalidSource => formatter.write_str("InvalidSource"),
            Self::Upstream { status } => formatter
                .debug_struct("Upstream")
                .field("status", status)
                .finish(),
            Self::TransportUnavailable => formatter.write_str("TransportUnavailable"),
        }
    }
}

#[derive(Clone)]
struct CachedProfileAvatarSource {
    source: String,
    expires_at: Instant,
}

pub struct CodexCredentialProfileService {
    repository: CodexCredentialRepository,
    profile: CodexWireProfileState,
    http: Client,
    base_url: String,
    avatar_sources: Mutex<HashMap<ProviderAccountId, CachedProfileAvatarSource>>,
}

impl CodexCredentialProfileService {
    #[must_use]
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: Client,
        base_url: String,
    ) -> Self {
        Self {
            repository,
            profile,
            http,
            base_url,
            avatar_sources: Mutex::new(HashMap::new()),
        }
    }

    pub async fn profile_statistics(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexProfileStatistics, CodexProfileStatisticsError> {
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|error| CodexProfileStatisticsError::Store {
                detail: error.to_string(),
            })?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexProfileStatisticsError::NotFound)?;
        if account
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at <= SystemTime::now())
        {
            return Err(CodexProfileStatisticsError::CredentialRefreshRequired {
                upstream_body: None,
            });
        }
        let credential = self
            .repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| CodexProfileStatisticsError::InvalidCredentialData)?;
        let authorization = credential
            .authentication
            .authorization_header()
            .map_err(|_| CodexProfileStatisticsError::InvalidCredentialData)?;
        let request_id = format!("profile_statistics_{}", Uuid::now_v7().simple());
        let statistics = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        )
        .fetch_profile_statistics(CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            account.upstream_account_id(),
            &request_id,
            None,
        ))
        .await
        .map_err(map_client_error)?;
        self.remember_avatar_source(account_id, statistics.image_url.as_deref());
        Ok(statistics)
    }

    pub async fn profile_avatar(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexProfileAvatar, CodexProfileAvatarError> {
        let source = match self.cached_avatar_source(account_id) {
            Some(source) => source,
            None => self
                .profile_statistics(account_id)
                .await?
                .image_url
                .ok_or(CodexProfileAvatarError::Missing)?,
        };
        let desktop_user_agent = self.profile.snapshot().desktop_user_agent();
        fetch_profile_avatar(&self.http, &self.base_url, &desktop_user_agent, &source)
            .await
            .map_err(map_avatar_fetch_error)
    }

    fn cached_avatar_source(&self, account_id: &ProviderAccountId) -> Option<String> {
        let now = Instant::now();
        let mut sources = self
            .avatar_sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sources.retain(|_, source| source.expires_at > now);
        sources.get(account_id).map(|source| source.source.clone())
    }

    fn remember_avatar_source(&self, account_id: &ProviderAccountId, source: Option<&str>) {
        let now = Instant::now();
        let mut sources = self
            .avatar_sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sources.retain(|_, source| source.expires_at > now);
        match source {
            Some(source) => {
                sources.insert(
                    account_id.clone(),
                    CachedProfileAvatarSource {
                        source: source.to_owned(),
                        expires_at: now + PROFILE_AVATAR_SOURCE_TTL,
                    },
                );
            }
            None => {
                sources.remove(account_id);
            }
        }
    }
}

fn map_avatar_fetch_error(error: CodexProfileAvatarFetchError) -> CodexProfileAvatarError {
    match error {
        CodexProfileAvatarFetchError::InvalidSource => CodexProfileAvatarError::InvalidSource,
        CodexProfileAvatarFetchError::Upstream { status } => {
            CodexProfileAvatarError::Upstream { status }
        }
        CodexProfileAvatarFetchError::TransportUnavailable => {
            CodexProfileAvatarError::TransportUnavailable
        }
    }
}

fn map_client_error(error: CodexClientError) -> CodexProfileStatisticsError {
    match error {
        CodexClientError::Upstream { status, body, .. }
            if status == reqwest::StatusCode::UNAUTHORIZED =>
        {
            CodexProfileStatisticsError::CredentialRefreshRequired {
                upstream_body: Some(body),
            }
        }
        CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
            ..
        } => CodexProfileStatisticsError::Upstream {
            status: status.as_u16(),
            body,
            retry_after_seconds,
        },
        _ => CodexProfileStatisticsError::TransportUnavailable,
    }
}
