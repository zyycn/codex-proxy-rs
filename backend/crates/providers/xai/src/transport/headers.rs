use std::fmt;

use gateway_core::engine::ModelRequestId;
use gateway_core::routing::UpstreamModelId;
use uuid::Uuid;

use crate::{SecretValue, XaiWireProfileState};

use super::SelectedGrokSession;

/// 进程级 Grok Build 客户端身份；请求级身份在构造 headers 时单独生成。
#[derive(Clone)]
pub struct GrokClientIdentity(SecretValue);

impl GrokClientIdentity {
    #[must_use]
    pub fn new() -> Self {
        Self(SecretValue::new(Uuid::new_v4().hyphenated().to_string()))
    }

    const fn value(&self) -> &SecretValue {
        &self.0
    }
}

impl Default for GrokClientIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for GrokClientIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrokClientIdentity([PSEUDONYM])")
    }
}

/// Public or sensitive official Grok request header value.
#[derive(Clone)]
pub enum GrokHeaderValue {
    /// Non-sensitive protocol metadata.
    Public(String),
    /// OAuth, identity, or session value that must be redacted.
    Sensitive(SecretValue),
}

impl GrokHeaderValue {
    /// Exposes a value only at the injected HTTP transport boundary.
    #[must_use]
    pub fn expose(&self) -> &str {
        match self {
            Self::Public(value) => value,
            Self::Sensitive(value) => value.expose(),
        }
    }

    /// Reports whether the value must be excluded from logs and diagnostics.
    #[must_use]
    pub const fn is_sensitive(&self) -> bool {
        matches!(self, Self::Sensitive(_))
    }
}

impl fmt::Debug for GrokHeaderValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public(value) => formatter.debug_tuple("Public").field(value).finish(),
            Self::Sensitive(_) => formatter.write_str("Sensitive([REDACTED])"),
        }
    }
}

/// One official Grok CLI proxy request header.
#[derive(Debug, Clone)]
pub struct GrokHeader {
    name: &'static str,
    value: GrokHeaderValue,
}

impl GrokHeader {
    /// Returns the static header name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the typed header value.
    #[must_use]
    pub const fn value(&self) -> &GrokHeaderValue {
        &self.value
    }

    pub(crate) fn public(name: &'static str, value: impl Into<String>) -> Self {
        Self {
            name,
            value: GrokHeaderValue::Public(value.into()),
        }
    }

    pub(crate) fn sensitive(name: &'static str, value: SecretValue) -> Self {
        Self {
            name,
            value: GrokHeaderValue::Sensitive(value),
        }
    }
}

pub fn build_grok_headers(
    profile: &XaiWireProfileState,
    session: &SelectedGrokSession,
    client_identity: &GrokClientIdentity,
    request_id: &ModelRequestId,
    upstream_session_id: Option<&str>,
    turn_index: Option<&str>,
    model: &UpstreamModelId,
) -> Vec<GrokHeader> {
    let upstream_request_id = Uuid::new_v4().hyphenated().to_string();
    let trace_id = Uuid::new_v4().simple().to_string();
    let span_source = Uuid::new_v4().simple().to_string();
    let traceparent = format!("00-{trace_id}-{}-01", &span_source[..16]);

    let mut headers = vec![
        GrokHeader::sensitive(
            "authorization",
            SecretValue::new(format!("Bearer {}", session.access_token().expose())),
        ),
        GrokHeader::public("X-XAI-Token-Auth", "xai-grok-cli"),
        GrokHeader::public("x-authenticateresponse", "authenticate-response"),
        GrokHeader::sensitive("x-grok-user-id", session.user_id().clone()),
        GrokHeader::public("x-grok-client-version", profile.client_version()),
        GrokHeader::public("x-grok-client-mode", profile.client_mode()),
        GrokHeader::public("x-grok-client-identifier", profile.client_identifier()),
        GrokHeader::public("user-agent", profile.user_agent()),
        GrokHeader::public("content-type", "application/json"),
        GrokHeader::public("accept", "text/event-stream"),
        GrokHeader::public("accept-encoding", "identity"),
        GrokHeader::sensitive("x-grok-req-id", SecretValue::new(upstream_request_id)),
        GrokHeader::sensitive(
            "idempotency-key",
            SecretValue::new(request_id.as_str().to_owned()),
        ),
        GrokHeader::public("traceparent", traceparent),
        GrokHeader::public("x-grok-model-override", model.as_str()),
        GrokHeader::sensitive("x-grok-agent-id", client_identity.value().clone()),
    ];
    if let Some(upstream_session_id) = upstream_session_id {
        headers.push(GrokHeader::sensitive(
            "x-grok-conv-id",
            SecretValue::new(upstream_session_id.to_owned()),
        ));
        if let Some(turn_index) = turn_index {
            headers.push(GrokHeader::sensitive(
                "x-grok-session-id",
                SecretValue::new(upstream_session_id.to_owned()),
            ));
            headers.push(GrokHeader::public("x-grok-turn-idx", turn_index));
        }
    }
    headers
}
