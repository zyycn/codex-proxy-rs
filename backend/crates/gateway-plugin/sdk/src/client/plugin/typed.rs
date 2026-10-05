//! 插件方法的类型化调用与响应编解码，以及调用阶段校验

use std::marker::PhantomData;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{CallContext, Capability, ErrorCode, PluginFault, Stage};

use super::super::{CallCancellation, CallReply, HostClient, PluginCall, ResponseStream};

/// RPC 的空控制对象，不使用会编码为 `null` 的 Rust 单元类型
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

pub(super) type Decoder<P> = fn(PluginCall, &'static [Stage]) -> Result<TypedCall<P>, PluginFault>;
pub(super) type Encoder<R> = fn(TypedReply<R>) -> Result<CallReply, PluginFault>;

/// SDK 方法标识与请求／响应类型；只能使用 [`super::methods`] 中的合同常量
pub struct Method<P, R> {
    pub(super) name: &'static str,
    pub(super) capabilities: &'static [Capability],
    pub(super) stages: &'static [Stage],
    pub(super) decode: Decoder<P>,
    pub(super) encode: Encoder<R>,
    marker: PhantomData<fn(P) -> R>,
}

impl<P, R> Copy for Method<P, R> {}

impl<P, R> Clone for Method<P, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P, R> Method<P, R> {
    pub(super) const fn new(
        name: &'static str,
        capabilities: &'static [Capability],
        stages: &'static [Stage],
        decode: Decoder<P>,
        encode: Encoder<R>,
    ) -> Self {
        Self {
            name,
            capabilities,
            stages,
            decode,
            encode,
            marker: PhantomData,
        }
    }
}

/// 业务请求、宿主上下文与当前父调用内的服务句柄；不实现内容型 `Debug`
pub struct TypedCall<P> {
    pub request: P,
    /// 只有合同明确保留原始正文的方法才会在此提供字节
    pub payload: Vec<u8>,
    pub context: CallContext,
    pub host: HostClient,
    pub cancellation: CallCancellation,
}

/// 类型化结果，可复用现有有界响应流；敏感 JSON 按方法合同写入二进制载荷
pub struct TypedReply<R> {
    pub result: R,
    pub payload: Vec<u8>,
    pub stream: Option<ResponseStream>,
}

impl<R> TypedReply<R> {
    #[must_use]
    pub const fn new(result: R) -> Self {
        Self {
            result,
            payload: Vec::new(),
            stream: None,
        }
    }

    #[must_use]
    pub fn with_payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    #[must_use]
    pub fn with_stream(mut self, stream: ResponseStream) -> Self {
        self.stream = Some(stream);
        self
    }
}

pub(super) fn decode_metadata<P: DeserializeOwned>(
    mut call: PluginCall,
    stages: &'static [Stage],
) -> Result<TypedCall<P>, PluginFault> {
    validate_stage(&call, stages)?;
    let request =
        serde_json::from_value(std::mem::take(&mut call.params)).map_err(|_| invalid_input())?;
    Ok(typed_call(call, request, true))
}

pub(super) fn decode_metadata_without_payload<P: DeserializeOwned>(
    mut call: PluginCall,
    stages: &'static [Stage],
) -> Result<TypedCall<P>, PluginFault> {
    validate_stage(&call, stages)?;
    if !call.payload.is_empty() {
        return Err(invalid_input());
    }
    let request =
        serde_json::from_value(std::mem::take(&mut call.params)).map_err(|_| invalid_input())?;
    Ok(typed_call(call, request, false))
}

pub(super) fn decode_payload<P: DeserializeOwned>(
    call: PluginCall,
    stages: &'static [Stage],
) -> Result<TypedCall<P>, PluginFault> {
    validate_stage(&call, stages)?;
    if !empty_object(&call.params) {
        return Err(invalid_input());
    }
    let request = serde_json::from_slice(&call.payload).map_err(|_| invalid_input())?;
    Ok(typed_call(call, request, false))
}

pub(super) fn decode_upstream(
    mut call: PluginCall,
    stages: &'static [Stage],
) -> Result<TypedCall<crate::call::upstream_adapter::UpstreamAdapterRequest>, PluginFault> {
    validate_stage(&call, stages)?;
    if !empty_object(&call.params) {
        return Err(invalid_input());
    }
    let (request, body) =
        crate::call::upstream_adapter::UpstreamAdapterRequest::decode(&call.payload)
            .map_err(|_| invalid_input())?;
    call.payload = body;
    Ok(typed_call(call, request, true))
}

pub(super) fn encode_metadata<R: Serialize>(
    reply: TypedReply<R>,
) -> Result<CallReply, PluginFault> {
    let TypedReply {
        result,
        payload,
        stream,
    } = reply;
    if !payload.is_empty() || stream.is_some() {
        return Err(invalid_input());
    }
    let result = serde_json::to_value(result).map_err(|_| invalid_input())?;
    Ok(CallReply::unary(result, Vec::new()))
}

pub(super) fn encode_stream<R: Serialize>(reply: TypedReply<R>) -> Result<CallReply, PluginFault> {
    let TypedReply {
        result,
        payload,
        stream,
    } = reply;
    if !payload.is_empty() {
        return Err(invalid_input());
    }
    Ok(CallReply::stream(
        serde_json::to_value(result).map_err(|_| invalid_input())?,
        payload,
        stream.ok_or_else(invalid_input)?,
    ))
}

pub(super) fn encode_metadata_with_payload<R: Serialize>(
    reply: TypedReply<R>,
) -> Result<CallReply, PluginFault> {
    let TypedReply {
        result,
        payload,
        stream,
    } = reply;
    if stream.is_some() {
        return Err(invalid_input());
    }
    let result = serde_json::to_value(result).map_err(|_| invalid_input())?;
    Ok(CallReply::unary(result, payload))
}

pub(super) fn encode_payload<R: Serialize>(reply: TypedReply<R>) -> Result<CallReply, PluginFault> {
    let TypedReply {
        result,
        payload,
        stream,
    } = reply;
    if !payload.is_empty() || stream.is_some() {
        return Err(invalid_input());
    }
    let payload = serde_json::to_vec(&result).map_err(|_| invalid_input())?;
    Ok(CallReply::unary(serde_json::json!({}), payload))
}

fn typed_call<P>(mut call: PluginCall, request: P, keep_payload: bool) -> TypedCall<P> {
    let payload = if keep_payload {
        std::mem::take(&mut call.payload)
    } else {
        Vec::new()
    };
    TypedCall {
        request,
        payload,
        context: call.context,
        host: call.host,
        cancellation: call.cancellation,
    }
}

fn validate_stage(call: &PluginCall, stages: &[Stage]) -> Result<(), PluginFault> {
    if stages.contains(&call.context.stage) {
        Ok(())
    } else {
        Err(invalid_input())
    }
}

fn empty_object(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

pub(super) fn invalid_input() -> PluginFault {
    PluginFault::new(
        ErrorCode::InvalidInput,
        "plugin typed method input or result is invalid",
    )
}
