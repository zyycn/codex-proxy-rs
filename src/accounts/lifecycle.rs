#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountLifecycleEvent {
    Added,
    Refreshed,
    Expired,
    QuotaExhausted,
    Banned,
    Disabled,
}
