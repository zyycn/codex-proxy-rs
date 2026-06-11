#[derive(Debug, Clone, Copy)]
pub struct RefreshPolicy {
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
}
