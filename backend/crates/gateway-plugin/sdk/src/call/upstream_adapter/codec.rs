//! 上游适配请求与事件的元数据、正文及续接信息编解码

use serde::{Deserialize, Serialize};

use super::{UpstreamAdapterEvent, UpstreamAdapterRequest, UpstreamContinuation, UpstreamFailure};
use crate::call::model::{
    ExecutionEncodingError, ExecutionEvent,
    codec::{pack, pack_with_limit, unpack, unpack_with_limit},
};

#[derive(Serialize, Deserialize)]
struct Metadata {
    observation: Option<super::super::model::facts::ResponseObservation>,
    continuation: Option<UpstreamContinuation>,
    failure: Option<UpstreamFailure>,
}

pub(super) fn encode(value: UpstreamAdapterEvent) -> Result<Vec<u8>, ExecutionEncodingError> {
    let metadata = Metadata {
        observation: value.observation,
        continuation: value.continuation,
        failure: value.failure,
    };
    pack(*b"GPA1", &metadata, [value.event.encode()?])
}

pub(super) fn decode(bytes: &[u8]) -> Result<UpstreamAdapterEvent, ExecutionEncodingError> {
    let (metadata, [event]): (Metadata, _) = unpack(*b"GPA1", bytes)?;
    Ok(UpstreamAdapterEvent {
        event: ExecutionEvent::decode(event)?,
        observation: metadata.observation,
        continuation: metadata.continuation,
        failure: metadata.failure,
    })
}

pub(super) fn encode_request(
    request: UpstreamAdapterRequest,
    body: Vec<u8>,
) -> Result<Vec<u8>, ExecutionEncodingError> {
    // 请求预算由宿主配置检查，线协议只约束 u32 长度；响应事件仍受独立预算限制
    pack_with_limit(*b"GPAQ", &request, [body], usize::MAX)
}

pub(super) fn decode_request(
    bytes: &[u8],
) -> Result<(UpstreamAdapterRequest, Vec<u8>), ExecutionEncodingError> {
    let (request, [body]) = unpack_with_limit(*b"GPAQ", bytes, usize::MAX)?;
    Ok((request, body.to_vec()))
}
