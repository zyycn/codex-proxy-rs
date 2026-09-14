//! 各官方调用链的请求头契约；版本与平台信息统一来自运行时画像快照。

use gateway_protocol::openai::{
    X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER, X_OPENAI_MEMGEN_REQUEST_HEADER,
};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT,
};

use super::client::{
    CodexBackendClient, CodexClientResult, CodexRequestContext, openai_subagent_from_metadata,
};
use super::profile::{CodexResidency, CodexWireProfile};
use super::protocol::responses::CodexResponsesRequest;

const CODEX_RESIDENCY_HEADER: &str = "x-openai-internal-codex-residency";

/// 构造 Codex Core 为模型请求设置的稳定身份请求头。
pub fn build_codex_model_headers(
    profile: &CodexWireProfile,
    authorization: &str,
    account_id: Option<&str>,
) -> CodexClientResult<HeaderMap> {
    let mut headers = build_codex_profile_headers(profile)?;
    headers.insert(
        HeaderName::from_static("version"),
        HeaderValue::from_str(&profile.codex_version)?,
    );
    headers.insert(AUTHORIZATION, HeaderValue::from_str(authorization)?);
    insert_optional_header(&mut headers, "chatgpt-account-id", account_id)?;
    Ok(headers)
}

/// Core 默认客户端身份，用于模型接口及 OAuth refresh；raw token exchange 不使用。
pub fn build_codex_profile_headers(profile: &CodexWireProfile) -> CodexClientResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_str(&profile.originator)?,
    );
    headers.insert(USER_AGENT, HeaderValue::from_str(&profile.user_agent())?);
    if let Some(CodexResidency::Us) = profile.residency {
        headers.insert(
            HeaderName::from_static(CODEX_RESIDENCY_HEADER),
            HeaderValue::from_static("us"),
        );
    }
    Ok(headers)
}

/// Core backend-client 的账号请求：不携带模型版本或 originator。
pub fn build_codex_account_headers(
    profile: &CodexWireProfile,
    authorization: &str,
    account_id: Option<&str>,
) -> CodexClientResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_str(&profile.user_agent())?);
    headers.insert(AUTHORIZATION, HeaderValue::from_str(authorization)?);
    insert_optional_header(&mut headers, "chatgpt-account-id", account_id)?;
    Ok(headers)
}

/// 桌面下载的账号身份；完整性凭据须由认证主体持有，不能借用下游客户端的值。
pub fn build_codex_download_headers(
    profile: &CodexWireProfile,
    authorization: &str,
    account_id: Option<&str>,
) -> CodexClientResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&profile.desktop_user_agent())?,
    );
    headers.insert(AUTHORIZATION, HeaderValue::from_str(authorization)?);
    insert_optional_header(&mut headers, "chatgpt-account-id", account_id)?;
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_str(&profile.originator)?,
    );
    Ok(headers)
}

impl CodexBackendClient {
    pub(super) fn model_request_headers(
        &self,
        profile: &CodexWireProfile,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers =
            build_codex_model_headers(profile, context.authorization, context.account_id)?;
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        Ok(headers)
    }

    pub(super) fn account_request_headers(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = build_codex_account_headers(
            &self.profile.snapshot(),
            context.authorization,
            context.account_id,
        )?;
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        Ok(headers)
    }

    pub(super) fn request_headers_for_http_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.response_headers(request, context)?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_optional_protocol_header(
            &mut headers,
            X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER,
            request.responses_lite.as_deref(),
        );
        Ok(headers)
    }

    pub(super) fn request_headers_for_websocket_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.response_headers(request, context)?;
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses_websockets=2026-02-06"),
        );
        Ok(headers)
    }

    /// HTTP 和 WS 共用的 Core 会话字段；传输专属字段由各自入口生成。
    fn response_headers(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.model_request_headers(&self.profile.snapshot(), context)?;
        headers.insert(
            HeaderName::from_static("x-client-request-id"),
            HeaderValue::from_str(context.request_id)?,
        );
        insert_optional_protocol_header(
            &mut headers,
            "x-client-request-id",
            context
                .client_request_id
                .or(context.thread_id)
                .or(context.session_id),
        );
        for (name, value) in [
            ("session-id", context.session_id),
            ("thread-id", context.thread_id),
            ("x-codex-window-id", context.codex_window_id),
            ("x-codex-turn-state", context.turn_state),
            ("x-codex-turn-metadata", context.turn_metadata),
            ("x-codex-beta-features", context.beta_features),
            (
                "x-responsesapi-include-timing-metrics",
                context.include_timing_metrics,
            ),
            ("x-codex-parent-thread-id", context.parent_thread_id),
            (
                X_OPENAI_MEMGEN_REQUEST_HEADER,
                request.memgen_request.as_deref(),
            ),
        ] {
            insert_optional_protocol_header(&mut headers, name, value);
        }
        if let Some(subagent) = openai_subagent_from_metadata(request.client_metadata()) {
            insert_optional_protocol_header(&mut headers, "x-openai-subagent", Some(&subagent));
        }
        let routing_hint = match request.service_tier() {
            Some(tier) => format!("model={};tier={tier}", request.model()),
            None => format!("model={}", request.model()),
        };
        insert_optional_protocol_header(&mut headers, "x-codex-routing-hint", Some(&routing_hint));
        for name in request.passthrough_headers.keys() {
            // 身份与传输字段只由画像/正文生成；其余协议头保留原始多值字节。
            if matches!(
                name.as_str(),
                "originator"
                    | "user-agent"
                    | "version"
                    | "authorization"
                    | "chatgpt-account-id"
                    | "cookie"
                    | "x-openai-internal-codex-residency"
                    | "openai-beta"
                    | "accept"
                    | "content-type"
                    | "content-encoding"
                    | "x-codex-routing-hint"
                    | "x-codex-installation-id"
                    | "x-codex-turn-id"
                    | "x-oai-attestation"
                    | "x-oai-is"
                    | "x-oai-is-update"
                    | X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER
            ) {
                continue;
            }
            headers.remove(name);
            for value in request.passthrough_headers.get_all(name) {
                headers.append(name.clone(), value.clone());
            }
        }
        Ok(headers)
    }
}

pub(super) fn insert_optional_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: Option<&str>,
) -> CodexClientResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    headers.insert(HeaderName::from_static(name), HeaderValue::from_str(value)?);
    Ok(())
}

/// 尽力投影客户端协议值；无法表示为 HTTP header 时保留正文并跳过投影。
pub(super) fn insert_optional_protocol_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: Option<&str>,
) {
    let Some(value) = value.and_then(|value| HeaderValue::from_str(value).ok()) else {
        return;
    };
    headers.insert(HeaderName::from_static(name), value);
}

fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        // tungstenite 的 opening serializer 只接受可 `to_str()` 的值；逐条跳过
        // 无法构造的头，不能让一个扩展头中断业务 payload。
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

pub(super) fn websocket_header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut pairs = header_pairs(headers);
    // WebSocket opening 的 HeaderMap 先由业务头构造，再被 tungstenite 插入协议头；
    // 这里复现官方 HeaderMap 交给 tungstenite 时的迭代顺序，最终序列化后的
    // 线级顺序由 fingerprint 测试锁定。未知扩展头仍保持相对顺序。
    pairs.sort_by_key(|(name, _)| websocket_header_order(name));
    pairs
}

fn websocket_header_order(name: &str) -> usize {
    const OFFICIAL_INSERTION_ORDER: &[&str] = &[
        "version",
        "x-codex-beta-features",
        "x-client-request-id",
        "session-id",
        "thread-id",
        "x-codex-window-id",
        "x-codex-turn-metadata",
        "x-codex-routing-hint",
        "openai-beta",
        "originator",
        "user-agent",
        "authorization",
        "chatgpt-account-id",
    ];
    OFFICIAL_INSERTION_ORDER
        .iter()
        .position(|candidate| name.eq_ignore_ascii_case(candidate))
        .unwrap_or(OFFICIAL_INSERTION_ORDER.len())
}
