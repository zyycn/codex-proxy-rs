//! 为测试子进程严格解码上游适配器 v1 的请求元数据

use gateway_plugin_sdk::call::upstream_adapter::{
    BuiltinProvider, UpstreamAdapterRequest, UpstreamContinuation,
};
use serde::Deserialize;

// 固定 v1 的严格解码器，不能用新版 DTO 的宽松解析掩盖旧插件加载失败
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    adapter_id: String,
    provider: BuiltinProvider,
    upstream_model: String,
    client_key_id: String,
    account_id: String,
    credential_revision: u64,
    protocol: String,
    client_transport: String,
    disable_fast: bool,
    headers: Vec<(String, Vec<u8>)>,
    continuation: Option<UpstreamContinuation>,
}

pub fn decode(payload: &[u8], expected_disabled: bool) -> (UpstreamAdapterRequest, Vec<u8>) {
    assert_eq!(&payload[..4], b"GPAQ");
    let metadata_length = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as usize;
    let body_length = u32::from_be_bytes(payload[8..12].try_into().unwrap()) as usize;
    assert_eq!(payload.len(), 12 + metadata_length + body_length);
    let old: Request = serde_json::from_slice(&payload[12..12 + metadata_length]).unwrap();
    assert_eq!(old.disable_fast, expected_disabled);
    (
        UpstreamAdapterRequest {
            adapter_id: old.adapter_id,
            provider: old.provider,
            upstream_model: old.upstream_model,
            client_key_id: old.client_key_id,
            account_id: old.account_id,
            credential_revision: old.credential_revision,
            protocol: old.protocol,
            client_transport: old.client_transport,
            fast_mode: if old.disable_fast {
                "disabled"
            } else {
                "default"
            }
            .into(),
            headers: old.headers,
            continuation: old.continuation,
        },
        payload[12 + metadata_length..].to_vec(),
    )
}
