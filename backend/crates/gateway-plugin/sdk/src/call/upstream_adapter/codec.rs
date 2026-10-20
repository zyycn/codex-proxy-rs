//! 上游适配请求与事件的元数据、正文及续接信息编解码

use serde::{Deserialize, Serialize};

use super::{UpstreamAdapterEvent, UpstreamAdapterRequest, UpstreamContinuation, UpstreamFailure};
use crate::call::model::{
    ExecutionEncodingError, ExecutionEvent,
    codec::{pack, unpack},
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
    pack(*b"GPAQ", &request, [body])
}

pub(super) fn decode_request(
    bytes: &[u8],
) -> Result<(UpstreamAdapterRequest, Vec<u8>), ExecutionEncodingError> {
    let (request, [body]) = unpack(*b"GPAQ", bytes)?;
    Ok((request, body.to_vec()))
}
