//! 账号耗尽状态聚合。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExhaustedAccountKind {
    QuotaExhausted,
    RateLimited,
    Expired,
    Disabled,
    Banned,
    CloudflareChallenge,
    CloudflarePathBlocked,
    ModelUnsupported,
    UpstreamUnavailable,
}

/// Controller 交给 attempt runner 聚合的单账号耗尽事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::dispatch) struct AccountExhaustionRecord {
    pub account_id: Option<String>,
    pub kind: ExhaustedAccountKind,
    pub upstream_error: String,
    pub status_code: Option<u16>,
}

impl AccountExhaustionRecord {
    pub(in crate::dispatch) fn new(
        account_id: impl Into<String>,
        kind: ExhaustedAccountKind,
        upstream_error: impl Into<String>,
    ) -> Self {
        Self {
            account_id: Some(account_id.into()),
            kind,
            upstream_error: upstream_error.into(),
            status_code: None,
        }
    }

    pub(in crate::dispatch) fn with_status_code(mut self, status_code: u16) -> Self {
        self.status_code = Some(status_code);
        self
    }
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
            Self::UpstreamUnavailable => "upstream-unavailable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    upstream_unavailable: ExhaustedCounter,
}

#[derive(Default)]
struct ExhaustedCounter {
    count: usize,
    upstream_error: Option<String>,
    status_code: Option<u16>,
}

impl AccountExhaustionTracker {
    pub(in crate::dispatch) fn record_exhaustion(&mut self, record: AccountExhaustionRecord) {
        self.record(
            record.account_id.as_deref(),
            record.kind,
            record.upstream_error,
            record.status_code,
        );
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

    pub(crate) fn last_account_id(&self) -> Option<&str> {
        self.last_account_id.as_deref()
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
            ExhaustedAccountKind::UpstreamUnavailable => &self.upstream_unavailable,
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
            ExhaustedAccountKind::UpstreamUnavailable => &mut self.upstream_unavailable,
        }
    }
}
