//! Antigravity 桌面端与 CLI 的模型选择器识别

use axum::http::{HeaderMap, header::USER_AGENT};

pub(super) fn uses_agent_menu(headers: &HeaderMap) -> bool {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|ua| ua.starts_with("antigravity/hub/") || ua.starts_with("antigravity/cli/"))
}
