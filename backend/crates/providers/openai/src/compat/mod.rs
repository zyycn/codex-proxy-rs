//! OpenAI 上下游协议差异与外部缺陷补偿的统一入口
//! 规则只处理请求事实；身份保护、会话状态和连接生命周期由对应职责模块维护

mod headers;
mod messages;
mod request;
mod search_history;
mod search_tools;

use gateway_core::engine::middleware::MiddlewareHeader;
use serde_json::{Map, Value};

pub(crate) use headers::is_non_codex_request_header;
pub(crate) use messages::add_translated_fast_beta;
pub(crate) use request::normalize_codex_request_body;
pub(crate) use search_tools::declares_hosted_search;

/// 选定 Codex/OAuth 上游后处理客户端正文兼容
pub(crate) fn normalize_selected_codex_downstream_body(
    body: &mut Map<String, Value>,
    responses_lite: bool,
    middleware_headers: &[MiddlewareHeader],
    request_id: &str,
) {
    request::normalize_non_codex_request_body(body);
    search_history::normalize_search_history(body, responses_lite, middleware_headers, request_id);
}
