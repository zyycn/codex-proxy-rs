//! Cookie 捕获与重放策略。

/// 单个 Cookie 条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieEntry {
    /// Cookie 名称。
    pub name: String,
    /// Cookie 值。
    pub value: String,
}

/// 将 Cookie 条目组装为 HTTP Cookie 头。
pub fn cookie_header(cookies: &[CookieEntry]) -> Option<String> {
    let header = cookies
        .iter()
        .filter(|cookie| !cookie.name.trim().is_empty())
        .map(|cookie| format!("{}={}", cookie.name.trim(), cookie.value))
        .collect::<Vec<_>>()
        .join("; ");
    (!header.is_empty()).then_some(header)
}
