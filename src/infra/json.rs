//! 无业务含义的 JSON 与分页辅助。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Serialize;

// ---------------------------------------------------------------------------
// 分页
// ---------------------------------------------------------------------------

/// 通用分页结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// 当前页条目。
    pub items: Vec<T>,
    /// 下一页游标。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 页码分页结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberedPage<T> {
    /// 当前页条目。
    pub items: Vec<T>,
    /// 总条目数。
    pub total: u64,
    /// 当前页码，从 1 开始。
    pub page: u32,
    /// 每页条目数。
    pub page_size: u32,
}

/// 编码游标。
pub fn encode_cursor(created_at: &str, id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{created_at}|{id}"))
}

/// 解码游标。
pub fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    let raw = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let (created_at, id) = text.split_once('|')?;
    Some((created_at.to_string(), id.to_string()))
}

/// 限制分页大小。
pub fn clamp_limit(limit: u32) -> u32 {
    limit.clamp(1, 200)
}

/// 规范化页码。
pub fn clamp_page(page: u32) -> u32 {
    page.max(1)
}

/// 计算页码分页偏移量。
pub fn page_offset(page: u32, page_size: u32) -> u64 {
    u64::from(clamp_page(page).saturating_sub(1)) * u64::from(clamp_limit(page_size))
}

/// 计算总页数。
pub fn total_pages(total: u64, page_size: u32) -> u32 {
    if total == 0 {
        return 0;
    }

    let page_size = u64::from(clamp_limit(page_size));
    let pages = total.div_ceil(page_size);
    pages.min(u64::from(u32::MAX)) as u32
}
