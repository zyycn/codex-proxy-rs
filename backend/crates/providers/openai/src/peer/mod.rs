//! API Key 上游互联：协议解析、持久观测与 Host 监督的后台同步

mod client;
mod projection;
mod scheduling;
mod service;
mod snapshot;
mod worker;

pub(crate) use service::PeerInfoService;
