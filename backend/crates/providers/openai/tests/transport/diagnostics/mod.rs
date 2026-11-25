//! 验证上游失败分类、重放资格、请求标识与响应事实提取

mod contract;
mod evidence;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use provider_openai::transport::CodexUpstreamDiagnostics;
use provider_openai::transport::diagnostics::{
    CodexFailureCategory, CodexUpstreamFailure, CodexUpstreamSendPhase,
};
use reqwest::header::{HeaderMap, HeaderValue};

fn classify(status: u16, body: &str) -> CodexFailureCategory {
    let status = reqwest::StatusCode::from_u16(status).expect("status code");
    CodexUpstreamFailure::from_response(
        status,
        body,
        None,
        &CodexUpstreamDiagnostics::with_status(status.as_u16()),
        None,
        &[],
        &[],
        CodexUpstreamSendPhase::AfterPayload,
    )
    .category()
}

fn trace_header<'a>(diagnostics: &'a CodexUpstreamDiagnostics, name: &str) -> Option<&'a str> {
    diagnostics
        .trace_headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
