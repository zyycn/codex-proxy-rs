//! 无业务含义的 JSON 与分页辅助。

use serde::Serialize;

/// 列表排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// 升序。
    Asc,
    /// 降序。
    Desc,
}

impl SortDirection {
    /// 解析管理端查询参数中的排序方向。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "asc" => Some(Self::Asc),
            "desc" => Some(Self::Desc),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 分页
// ---------------------------------------------------------------------------

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
