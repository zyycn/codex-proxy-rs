//! 固定的官方 Grok Build inference 配置。

pub const XAI_PROVIDER_NAME: &str = "xai";
pub const GROK_CLI_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const GROK_RESPONSES_URL: &str = "https://cli-chat-proxy.grok.com/v1/responses";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokProviderTransport {
    HttpSse,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrokProviderConfigError {
    #[error("official Grok Build Responses URL is invalid")]
    InvalidResponsesUrl,
}
