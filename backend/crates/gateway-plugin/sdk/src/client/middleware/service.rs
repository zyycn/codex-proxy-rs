//! 公开服务中间件；Operation 关联输入输出类型，无需逐操作增加 hook

use super::{MiddlewareInput, MiddlewareOutput, invalid_input};
use crate::client::session::{CallCancellation, CallReply, HostClient, PluginCall};
use crate::{
    CallContext, PluginFault, Stage,
    call::{
        middleware::NEXT_METHOD,
        services::{self as wire, Operation, ServiceError},
    },
};
use serde_json::Value;
use std::marker::PhantomData;

pub struct ServiceCall {
    pub context: CallContext,
    pub operation: String,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub request_id: String,
    input: Value,
    host: HostClient,
    pub cancellation: CallCancellation,
}

pub struct TypedServiceCall<O: Operation> {
    pub context: CallContext,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub request_id: String,
    pub input: O::Input,
    pub next: ServiceNext<O>,
    pub host: HostClient,
    pub cancellation: CallCancellation,
}

pub struct ServiceNext<O: Operation> {
    host: HostClient,
    marker: PhantomData<fn(O)>,
}

pub struct ServiceResponse(wire::Response);

impl ServiceCall {
    /// 不消费调用即可匹配其公开合同，再取得类型化视图
    #[must_use]
    pub fn is<O: Operation>(&self) -> bool {
        self.operation == O::NAME
    }

    pub fn into_typed<O: Operation>(self) -> Result<TypedServiceCall<O>, PluginFault> {
        if !self.is::<O>() {
            return Err(invalid_input());
        }
        Ok(TypedServiceCall {
            input: serde_json::from_value(self.input).map_err(|_| invalid_input())?,
            next: ServiceNext {
                host: self.host.clone(),
                marker: PhantomData,
            },
            host: self.host,
            context: self.context,
            call_id: self.call_id,
            parent_call_id: self.parent_call_id,
            request_id: self.request_id,
            cancellation: self.cancellation,
        })
    }

    pub async fn forward(self) -> Result<ServiceResponse, PluginFault> {
        let reply = self
            .host
            .call(NEXT_METHOD, self.input, Vec::new())
            .await
            .map_err(crate::client::session::SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty() {
            return Err(invalid_input());
        }
        serde_json::from_value(reply.result)
            .map(ServiceResponse)
            .map_err(|_| invalid_input())
    }
}

impl<O: Operation> ServiceNext<O> {
    pub async fn run(self, input: O::Input) -> Result<O::Output, ServiceError> {
        let input = serde_json::to_value(input).map_err(|_| invalid())?;
        decode(self.host.call(NEXT_METHOD, input, Vec::new()).await)
    }
}

impl ServiceResponse {
    pub fn from_result<O: Operation>(
        result: Result<O::Output, ServiceError>,
    ) -> Result<Self, PluginFault> {
        Ok(Self(match result {
            Ok(output) => Ok(serde_json::to_value(output).map_err(|_| invalid_input())?),
            Err(error) => Err(error),
        }))
    }
}

impl HostClient {
    pub async fn service<O: Operation>(&self, input: O::Input) -> Result<O::Output, ServiceError> {
        let params = serde_json::to_value(wire::Request {
            operation: O::NAME.into(),
            input: serde_json::to_value(input).map_err(|_| invalid())?,
        })
        .map_err(|_| invalid())?;
        decode(self.call(wire::CALL_METHOD, params, Vec::new()).await)
    }
}

fn decode<O: serde::de::DeserializeOwned>(
    reply: Result<crate::client::session::HostReply, crate::client::session::SessionError>,
) -> Result<O, ServiceError> {
    let reply = reply.map_err(|error| {
        let fault = error.into_plugin_fault();
        ServiceError {
            kind: "unavailable".into(),
            message: fault.message.clone(),
            details: Some(serde_json::json!(fault)),
        }
    })?;
    if !reply.payload.is_empty() {
        return Err(invalid());
    }
    let result: wire::Response = serde_json::from_value(reply.result).map_err(|_| invalid())?;
    serde_json::from_value(result?).map_err(|_| invalid())
}

fn invalid() -> ServiceError {
    ServiceError {
        kind: "invalid".into(),
        message: "公开服务输入或结果类型无效".into(),
        details: None,
    }
}

impl MiddlewareInput for ServiceCall {
    type Output = ServiceResponse;
    fn accepts(stage: Stage) -> bool {
        stage == Stage::Service
    }
    fn decode(call: PluginCall) -> Result<Self, PluginFault> {
        if !Self::accepts(call.context.stage) || !call.payload.is_empty() {
            return Err(invalid_input());
        }
        let input: wire::Call = serde_json::from_value(call.params).map_err(|_| invalid_input())?;
        Ok(Self {
            context: call.context,
            operation: input.operation,
            input: input.input,
            call_id: input.call_id,
            parent_call_id: input.parent_call_id,
            request_id: input.request_id,
            host: call.host,
            cancellation: call.cancellation,
        })
    }
}
impl MiddlewareOutput for ServiceResponse {
    fn encode(self) -> Result<CallReply, PluginFault> {
        Ok(CallReply::unary(
            serde_json::to_value(self.0).map_err(|_| invalid_input())?,
            Vec::new(),
        ))
    }
}
