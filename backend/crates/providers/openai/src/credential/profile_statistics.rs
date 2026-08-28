//! OpenAI 当前账号个人资料统计查询编排。

use std::time::SystemTime;

use gateway_core::engine::credential::ProviderAccountId;
use reqwest::Client;
use secrecy::ExposeSecret as _;
use thiserror::Error;
use uuid::Uuid;

use crate::transport::profile::CodexWireProfileState;
use crate::transport::{
    CodexBackendClient, CodexClientError, CodexProfileStatistics, CodexRequestContext,
};

use super::CodexCredentialRepository;

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

pub struct CodexCredentialProfileService {
    repository: CodexCredentialRepository,
    profile: CodexWireProfileState,
    http: Client,
    base_url: String,
}

impl CodexCredentialProfileService {
    #[must_use]
    pub const fn new(
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
        CodexBackendClient::new(
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
        .map_err(map_client_error)
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
