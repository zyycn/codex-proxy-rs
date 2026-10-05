//! 模型执行事件的元数据与原始载荷分离编码及有界解码

use serde::{Serialize, de::DeserializeOwned};

use super::{ExecutionEvent, WirePayload};

/// 宿主与插件还会施加更低的帧、队列及可用信用上限
pub const MAX_EXECUTION_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("plugin execution payload has invalid encoding or exceeds its limit")]
pub struct ExecutionEncodingError;

pub(super) fn encode_event(mut event: ExecutionEvent) -> Result<Vec<u8>, ExecutionEncodingError> {
    let wire = raw_wire(event.wire.as_mut())
        .map(std::mem::take)
        .unwrap_or_default();
    let origin = raw_wire(
        event
            .host
            .as_mut()
            .and_then(|host| host.middleware_origin_wire.as_mut()),
    )
    .map(std::mem::take)
    .unwrap_or_default();
    pack(*b"GPE2", &event, [wire, origin])
}

pub(super) fn decode_event(bytes: &[u8]) -> Result<ExecutionEvent, ExecutionEncodingError> {
    let (mut event, [wire, origin]): (ExecutionEvent, _) = unpack(*b"GPE2", bytes)?;
    attach(raw_wire(event.wire.as_mut()), wire)?;
    attach(
        raw_wire(
            event
                .host
                .as_mut()
                .and_then(|host| host.middleware_origin_wire.as_mut()),
        ),
        origin,
    )?;
    Ok(event)
}

pub(in crate::call) fn pack<T: Serialize, const N: usize>(
    prefix: [u8; 4],
    metadata: &T,
    payloads: [Vec<u8>; N],
) -> Result<Vec<u8>, ExecutionEncodingError> {
    let header_bytes = 4 + 4 * (N + 1);
    let raw_bytes = payloads
        .iter()
        .fold(0usize, |total, bytes| total.saturating_add(bytes.len()));
    if raw_bytes > MAX_EXECUTION_PAYLOAD_BYTES {
        return Err(ExecutionEncodingError);
    }
    let metadata = serde_json::to_vec(metadata).map_err(|_| ExecutionEncodingError)?;
    let total = header_bytes
        .saturating_add(metadata.len())
        .saturating_add(raw_bytes);
    if total > MAX_EXECUTION_PAYLOAD_BYTES {
        return Err(ExecutionEncodingError);
    }
    let mut result = Vec::with_capacity(total);
    result.extend_from_slice(&prefix);
    for length in std::iter::once(metadata.len()).chain(payloads.iter().map(Vec::len)) {
        result.extend_from_slice(
            &u32::try_from(length)
                .map_err(|_| ExecutionEncodingError)?
                .to_be_bytes(),
        );
    }
    result.extend_from_slice(&metadata);
    for bytes in payloads {
        result.extend_from_slice(&bytes);
    }
    Ok(result)
}

pub(in crate::call) fn unpack<T: DeserializeOwned, const N: usize>(
    prefix: [u8; 4],
    bytes: &[u8],
) -> Result<(T, [&[u8]; N]), ExecutionEncodingError> {
    let header_bytes = 4 + 4 * (N + 1);
    if bytes.len() < header_bytes
        || bytes.len() > MAX_EXECUTION_PAYLOAD_BYTES
        || bytes[..4] != prefix
    {
        return Err(ExecutionEncodingError);
    }
    let mut cursor = header_bytes;
    let mut metadata = &[][..];
    let mut payloads = [&[][..]; N];
    // 先验证全部分段边界，不能根据对端的长度声明直接分配
    for (chunk, target) in bytes[4..header_bytes]
        .chunks_exact(4)
        .zip(std::iter::once(&mut metadata).chain(payloads.iter_mut()))
    {
        let length = usize::try_from(u32::from_be_bytes(
            chunk.try_into().map_err(|_| ExecutionEncodingError)?,
        ))
        .map_err(|_| ExecutionEncodingError)?;
        let end = cursor.checked_add(length).ok_or(ExecutionEncodingError)?;
        *target = bytes.get(cursor..end).ok_or(ExecutionEncodingError)?;
        cursor = end;
    }
    if cursor != bytes.len() {
        return Err(ExecutionEncodingError);
    }
    let metadata = serde_json::from_slice(metadata).map_err(|_| ExecutionEncodingError)?;
    Ok((metadata, payloads))
}

fn attach(target: Option<&mut Vec<u8>>, bytes: &[u8]) -> Result<(), ExecutionEncodingError> {
    match target {
        Some(target) if target.is_empty() => target.extend_from_slice(bytes),
        None if bytes.is_empty() => {}
        _ => return Err(ExecutionEncodingError),
    }
    Ok(())
}

fn raw_wire(wire: Option<&mut super::WireEvent>) -> Option<&mut Vec<u8>> {
    match &mut wire?.payload {
        WirePayload::Json { raw_sse, .. } => raw_sse.as_mut(),
        WirePayload::RawSse { frame } => Some(frame),
        WirePayload::RawJson { body } | WirePayload::RawBody { body } => Some(body),
    }
}
