//! 下游客户端对 Codex Core/Desktop 请求协议的兼容处理。
//! 账号身份保护、会话规范化和 HTTP 传输规则由对应职责模块维护。

mod body;
mod headers;

pub(super) use body::normalize_codex_request_body;
pub(super) use headers::is_non_codex_request_header;
