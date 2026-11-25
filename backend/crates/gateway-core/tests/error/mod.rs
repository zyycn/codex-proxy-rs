//! 验证执行错误的敏感信息脱敏、稳定快照与请求局部数据边界

mod contract;
mod details;
mod source;
use bytes::Bytes;
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, GatewayError, OpaqueUpstreamValue,
    ProviderDiagnostic, ProviderError, ProviderErrorKind, RawUpstreamError,
};
use gateway_core::event::{ProtocolWireEvent, ProviderEvent, ProviderResponseHeader};
use gateway_core::upstream::UpstreamSendState;
use serde_json::json;
