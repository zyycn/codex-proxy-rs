use std::fmt;

use gateway_core::operation::GenerateRequest;
use serde_json::{Map, Value};

use super::XAI_PROVIDER_NAME;

const GROK_OPTION_FIELDS: &[&str] = &["schema_version", "transport"];
const IDENTITY_FIELDS: &[&str] = &[
    "Authorization",
    "authorization",
    "Cookie",
    "cookie",
    "accessToken",
    "access_token",
    "accountId",
    "account_id",
    "cookies",
    "email",
    "idToken",
    "id_token",
    "refreshToken",
    "refresh_token",
    "sessionToken",
    "session_token",
    "teamId",
    "team_id",
    "token",
    "userId",
    "user_id",
    "x-email",
    "x-grok-user-id",
    "x-userid",
];
const ACCOUNT_BOUND_FIELDS: &[&str] = &[
    "agentId",
    "agent_id",
    "conversation",
    "conversationId",
    "conversation_id",
    "previousResponseId",
    "previous_response_id",
    "responseId",
    "response_id",
    "sessionId",
    "session_id",
    "x-grok-agent-id",
    "x-grok-conv-id",
    "x-grok-session-id",
];

/// 保留客户端 OpenAI Responses object 的 xAI 上游请求。
pub struct GrokResponsesRequest {
    body: Map<String, Value>,
}

impl GrokResponsesRequest {
    /// 返回发送到 `/v1/responses` 的 JSON object。
    #[must_use]
    pub const fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    pub fn encode(
        request: &GenerateRequest,
        upstream_model: &str,
    ) -> Result<Self, GrokRequestEncodeError> {
        let payload = request
            .protocol_payload()
            .filter(|payload| payload.protocol() == "openai")
            .ok_or(GrokRequestEncodeError::InvalidProtocolPayload)?;
        let mut body = payload.body().clone();
        sanitize_account_identity(&mut body);
        body.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
        body.insert("stream".to_owned(), Value::Bool(true));
        body.insert("store".to_owned(), Value::Bool(false));
        if let Some(options) = request.provider_options().get(XAI_PROVIDER_NAME) {
            validate_provider_options(options)?;
        }
        Ok(Self { body })
    }

    pub(crate) fn to_json_bytes(&self) -> Result<Vec<u8>, GrokRequestEncodeError> {
        serde_json::to_vec(&self.body).map_err(|_| GrokRequestEncodeError::Serialization)
    }
}

impl fmt::Debug for GrokResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokResponsesRequest")
            .field("body_keys", &self.body.keys().collect::<Vec<_>>())
            .field("body", &"<prompt and tool payload redacted>")
            .finish()
    }
}

/// Generate-to-Responses encoding error that never retains option or prompt
/// values.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokRequestEncodeError {
    /// 数据面只接受 OpenAI adapter 保留的原始 Responses object。
    #[error("Grok Build request is missing its OpenAI protocol payload")]
    InvalidProtocolPayload,
    /// Grok provider option schema or value is malformed.
    #[error("Grok Build provider option schema is invalid")]
    InvalidProviderOptions,
    /// A Grok-specific option is unknown or unsupported.
    #[error("Grok Build provider option is unsupported")]
    UnsupportedProviderOption,
    /// JSON serialization unexpectedly failed.
    #[error("Grok Build request serialization failed")]
    Serialization,
}

fn validate_provider_options(options: &Map<String, Value>) -> Result<(), GrokRequestEncodeError> {
    if options
        .keys()
        .any(|field| !GROK_OPTION_FIELDS.contains(&field.as_str()))
    {
        return Err(GrokRequestEncodeError::UnsupportedProviderOption);
    }
    if options.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(GrokRequestEncodeError::InvalidProviderOptions);
    }
    if let Some(transport) = options.get("transport")
        && transport.as_str() != Some("http_sse")
    {
        return Err(GrokRequestEncodeError::InvalidProviderOptions);
    }
    Ok(())
}

fn sanitize_account_identity(body: &mut Map<String, Value>) {
    for field in IDENTITY_FIELDS.iter().chain(ACCOUNT_BOUND_FIELDS) {
        body.remove(*field);
    }
    let Some(Value::Object(metadata)) = body.get_mut("metadata") else {
        return;
    };
    for field in IDENTITY_FIELDS.iter().chain(ACCOUNT_BOUND_FIELDS) {
        metadata.remove(*field);
    }
}
