use std::fmt;

use gateway_core::engine::ModelRequestId;
use gateway_core::routing::UpstreamModelId;

use crate::SecretValue;

use super::{GrokProviderInstanceConfig, SelectedGrokSession};

const CLIENT_IDENTIFIER: &str = "codex-proxy-rs";
const CLIENT_MODE: &str = "headless";

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
    instance: &GrokProviderInstanceConfig,
    session: &SelectedGrokSession,
    request_id: &ModelRequestId,
    model: &UpstreamModelId,
) -> Vec<GrokHeader> {
    let request_id = request_id.as_str();

    let mut headers = vec![
        GrokHeader::sensitive(
            "authorization",
            SecretValue::new(format!("Bearer {}", session.access_token().expose())),
        ),
        GrokHeader::public("X-XAI-Token-Auth", "xai-grok-cli"),
        GrokHeader::public("x-authenticateresponse", "authenticate-response"),
        GrokHeader::sensitive("x-userid", session.user_id().clone()),
        GrokHeader::sensitive("x-grok-user-id", session.user_id().clone()),
        GrokHeader::public("x-grok-client-version", instance.client_version()),
        GrokHeader::public("x-grok-client-mode", CLIENT_MODE),
        GrokHeader::public("x-grok-client-identifier", CLIENT_IDENTIFIER),
        GrokHeader::public(
            "user-agent",
            format!("{CLIENT_IDENTIFIER}/{}", env!("CARGO_PKG_VERSION")),
        ),
        GrokHeader::public("content-type", "application/json"),
        GrokHeader::public("accept", "text/event-stream"),
        GrokHeader::sensitive("x-grok-conv-id", SecretValue::new(request_id.to_owned())),
        GrokHeader::sensitive("x-grok-req-id", SecretValue::new(request_id.to_owned())),
        GrokHeader::public("x-grok-model-override", model.as_str()),
        GrokHeader::sensitive("x-grok-session-id", SecretValue::new(request_id.to_owned())),
        GrokHeader::sensitive(
            "x-grok-agent-id",
            SecretValue::new(CLIENT_IDENTIFIER.to_owned()),
        ),
    ];
    if let Some(email) = session.email() {
        headers.push(GrokHeader::sensitive("x-email", email.clone()));
    }
    headers
}
