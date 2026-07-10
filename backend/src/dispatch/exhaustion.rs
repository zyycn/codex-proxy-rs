//! 账号耗尽状态聚合。

use crate::accounts::account::AccountStatus;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ExhaustedAccountKind {
    QuotaExhausted,
    RateLimited,
    Expired,
    Disabled,
    Banned,
    CloudflareChallenge,
    CloudflarePathBlocked,
    ModelUnsupported,
}

impl ExhaustedAccountKind {
    pub(crate) fn message_reason(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota-exhausted",
            Self::RateLimited => "rate-limited",
            Self::Expired => "expired",
            Self::Disabled => "disabled",
            Self::Banned => "banned",
            Self::CloudflareChallenge => "cloudflare-challenge",
            Self::CloudflarePathBlocked => "cloudflare-path-block",
            Self::ModelUnsupported => "model-unsupported",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ExhaustedAccount {
    pub kind: ExhaustedAccountKind,
    pub count: usize,
    pub upstream_error: String,
    pub status_code: Option<u16>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExhaustedAccountRef<'a> {
    pub kind: ExhaustedAccountKind,
    pub count: usize,
    pub upstream_error: &'a str,
}

#[derive(Default)]
pub(crate) struct AccountExhaustionTracker {
    last_kind: Option<ExhaustedAccountKind>,
    last_account_id: Option<String>,
    quota_exhausted: ExhaustedCounter,
    rate_limited: ExhaustedCounter,
    expired: ExhaustedCounter,
    disabled: ExhaustedCounter,
    banned: ExhaustedCounter,
    cloudflare_challenge: ExhaustedCounter,
    cloudflare_path_blocked: ExhaustedCounter,
    model_unsupported: ExhaustedCounter,
    model_unsupported_retry_used: bool,
}

#[derive(Default)]
struct ExhaustedCounter {
    count: usize,
    upstream_error: Option<String>,
    status_code: Option<u16>,
}

impl AccountExhaustionTracker {
    pub(crate) fn last_account_id(&self) -> Option<&str> {
        self.last_account_id.as_deref()
    }

    pub(crate) fn record_rate_limited(
        &mut self,
        account_id: Option<&str>,
        upstream_error: impl Into<String>,
    ) {
        self.record(
            account_id,
            ExhaustedAccountKind::RateLimited,
            upstream_error,
            None,
        );
    }

    pub(crate) fn record_quota_exhausted(
        &mut self,
        account_id: Option<&str>,
        upstream_error: impl Into<String>,
    ) {
        self.record(
            account_id,
            ExhaustedAccountKind::QuotaExhausted,
            upstream_error,
            None,
        );
    }

    pub(crate) fn record_auth_failure(
        &mut self,
        account_id: Option<&str>,
        account_status: AccountStatus,
        upstream_error: impl Into<String>,
        status_code: Option<u16>,
    ) {
        let kind = match account_status {
            AccountStatus::Disabled => ExhaustedAccountKind::Disabled,
            AccountStatus::Banned => ExhaustedAccountKind::Banned,
            _ => ExhaustedAccountKind::Expired,
        };
        self.record(account_id, kind, upstream_error, status_code);
    }

    pub(crate) fn record_cloudflare_challenge(
        &mut self,
        account_id: Option<&str>,
        upstream_error: impl Into<String>,
    ) {
        self.record(
            account_id,
            ExhaustedAccountKind::CloudflareChallenge,
            upstream_error,
            None,
        );
    }

    pub(crate) fn record_cloudflare_path_blocked(
        &mut self,
        account_id: Option<&str>,
        upstream_error: impl Into<String>,
    ) {
        self.record(
            account_id,
            ExhaustedAccountKind::CloudflarePathBlocked,
            upstream_error,
            None,
        );
    }

    pub(crate) fn record_model_unsupported(
        &mut self,
        account_id: Option<&str>,
        upstream_error: impl Into<String>,
    ) {
        self.model_unsupported_retry_used = true;
        self.record(
            account_id,
            ExhaustedAccountKind::ModelUnsupported,
            upstream_error,
            None,
        );
    }

    pub(crate) fn model_unsupported_retry_exhausted(
        &self,
        upstream_error: impl Into<String>,
    ) -> Option<ExhaustedAccount> {
        self.model_unsupported_retry_used.then(|| ExhaustedAccount {
            kind: ExhaustedAccountKind::ModelUnsupported,
            count: self.model_unsupported.count + 1,
            upstream_error: upstream_error.into(),
            status_code: None,
        })
    }

    pub(crate) fn last_exhausted(&self) -> Option<ExhaustedAccount> {
        let kind = self.last_kind?;
        let counter = self.counter(kind);
        Some(ExhaustedAccount {
            kind,
            count: counter.count,
            upstream_error: counter.upstream_error.clone().unwrap_or_default(),
            status_code: counter.status_code,
        })
    }

    fn record(
        &mut self,
        account_id: Option<&str>,
        kind: ExhaustedAccountKind,
        upstream_error: impl Into<String>,
        status_code: Option<u16>,
    ) {
        self.last_kind = Some(kind);
        if let Some(account_id) = account_id {
            self.last_account_id = Some(account_id.to_string());
        }
        let counter = self.counter_mut(kind);
        counter.count += 1;
        counter.upstream_error = Some(upstream_error.into());
        if let Some(status_code) = status_code {
            counter.status_code = Some(status_code);
        }
    }

    fn counter(&self, kind: ExhaustedAccountKind) -> &ExhaustedCounter {
        match kind {
            ExhaustedAccountKind::QuotaExhausted => &self.quota_exhausted,
            ExhaustedAccountKind::RateLimited => &self.rate_limited,
            ExhaustedAccountKind::Expired => &self.expired,
            ExhaustedAccountKind::Disabled => &self.disabled,
            ExhaustedAccountKind::Banned => &self.banned,
            ExhaustedAccountKind::CloudflareChallenge => &self.cloudflare_challenge,
            ExhaustedAccountKind::CloudflarePathBlocked => &self.cloudflare_path_blocked,
            ExhaustedAccountKind::ModelUnsupported => &self.model_unsupported,
        }
    }

    fn counter_mut(&mut self, kind: ExhaustedAccountKind) -> &mut ExhaustedCounter {
        match kind {
            ExhaustedAccountKind::QuotaExhausted => &mut self.quota_exhausted,
            ExhaustedAccountKind::RateLimited => &mut self.rate_limited,
            ExhaustedAccountKind::Expired => &mut self.expired,
            ExhaustedAccountKind::Disabled => &mut self.disabled,
            ExhaustedAccountKind::Banned => &mut self.banned,
            ExhaustedAccountKind::CloudflareChallenge => &mut self.cloudflare_challenge,
            ExhaustedAccountKind::CloudflarePathBlocked => &mut self.cloudflare_path_blocked,
            ExhaustedAccountKind::ModelUnsupported => &mut self.model_unsupported,
        }
    }
}
