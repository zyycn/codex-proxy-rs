//! 插件中间件的请求与响应视图，以及各调用边界的类型化接口

mod body;
mod call;
mod http;
mod plugin;
mod service;
mod websocket;

pub use body::{MiddlewareBody, MiddlewareBodySender};
pub use call::{MiddlewareCall, MiddlewareResult};
pub use http::{HttpBody, HttpCall, HttpFrame, HttpNext, HttpRequest, HttpResponse};
pub use plugin::MiddlewarePlugin;
pub use service::{ServiceCall, ServiceNext, ServiceResponse, TypedServiceCall};
pub use websocket::{
    WebSocketCall, WebSocketDirection, WebSocketKind, WebSocketMessage, WebSocketNext,
    WebSocketPayload, WebSocketSender, WebSocketSession,
};

use crate::{
    CallContext, ErrorCode, PluginFault, Stage,
    call::middleware::{
        HANDLE_METHOD, MiddlewareHeader, MiddlewareHeaderMutation, MiddlewareMount,
        MiddlewareNextRequest, MiddlewareNextResponse, MiddlewareRequestBody,
        MiddlewareRequestHead, MiddlewareResponseHead, NEXT_METHOD,
    },
};

use super::session::{CallCancellation, CallReply, HostClient, PluginCall, SessionError};

/// 作者可读取并修改的一次中间件请求
///
/// 直接修改公开的 `head`/`body` 会由 [`MiddlewareNext::run`] 与原始输入比较；
/// `replace_body` 还可明确表达“替换为空正文”
pub struct MiddlewareRequest {
    pub head: MiddlewareRequestHead,
    pub body: Vec<u8>,
    original_protocol: String,
    original_settings: serde_json::Value,
    original_headers: Vec<MiddlewareHeader>,
    original_body: Vec<u8>,
    body_replaced: bool,
    header_mutations: Vec<MiddlewareHeaderMutation>,
    capabilities: Option<crate::call::middleware::CapabilityDeclaration>,
}

impl MiddlewareRequest {
    /// 仅 request 阶段可声明转换；须同时替换正文并负责响应还原
    pub fn declare_capabilities(
        &mut self,
        declaration: crate::call::middleware::CapabilityDeclaration,
    ) {
        self.capabilities = Some(declaration);
    }

    /// 明确替换请求正文；空 Vec 也表示 replace-empty，而不是保留原正文
    pub fn replace_body(&mut self, body: Vec<u8>) {
        self.body = body;
        self.body_replaced = true;
    }

    /// 删除原始集合中同名的全部 header
    pub fn remove_header(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.head
            .headers
            .retain(|header| !header.name.eq_ignore_ascii_case(&name));
        self.header_mutations
            .push(MiddlewareHeaderMutation::Remove { name });
    }

    /// 在原始集合之后追加一个 header，保留同名多值
    pub fn append_header(&mut self, name: impl Into<String>, value: Vec<u8>) {
        let name = name.into();
        self.head.headers.push(MiddlewareHeader {
            name: name.clone(),
            value: value.clone(),
        });
        self.header_mutations
            .push(MiddlewareHeaderMutation::Append { name, value });
    }

    fn into_next_parts(self) -> (MiddlewareNextRequest, Vec<u8>) {
        let protocol = (self.head.protocol != self.original_protocol).then_some(self.head.protocol);
        let header_mutations = finish_header_mutations(
            &self.original_headers,
            self.header_mutations,
            self.head.headers,
        );
        let changed_body = self.body_replaced || self.body != self.original_body;
        let (body, payload) = if changed_body {
            (MiddlewareRequestBody::Replace, self.body)
        } else {
            (MiddlewareRequestBody::Preserve, Vec::new())
        };
        (
            MiddlewareNextRequest {
                settings: (self.head.settings != self.original_settings)
                    .then_some(self.head.settings),
                protocol,
                header_mutations,
                body,
                capabilities: self.capabilities,
            },
            payload,
        )
    }
}

fn replace_headers(
    original: &[MiddlewareHeader],
    replacement: Vec<MiddlewareHeader>,
) -> Vec<MiddlewareHeaderMutation> {
    let mut removed = Vec::<String>::new();
    for header in original {
        if !removed
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&header.name))
        {
            removed.push(header.name.clone());
        }
    }
    let mut mutations = removed
        .into_iter()
        .map(|name| MiddlewareHeaderMutation::Remove { name })
        .collect::<Vec<_>>();
    mutations.extend(
        replacement
            .into_iter()
            .map(|header| MiddlewareHeaderMutation::Append {
                name: header.name,
                value: header.value,
            }),
    );
    mutations
}

fn finish_header_mutations(
    original: &[MiddlewareHeader],
    mut mutations: Vec<MiddlewareHeaderMutation>,
    replacement: Vec<MiddlewareHeader>,
) -> Vec<MiddlewareHeaderMutation> {
    let mut projected = original.to_vec();
    for mutation in &mutations {
        match mutation {
            MiddlewareHeaderMutation::Remove { name } => {
                projected.retain(|header| !header.name.eq_ignore_ascii_case(name));
            }
            MiddlewareHeaderMutation::Append { name, value } => {
                projected.push(MiddlewareHeader {
                    name: name.clone(),
                    value: value.clone(),
                });
            }
        }
    }
    if projected != replacement {
        mutations.extend(replace_headers(&projected, replacement));
    }
    mutations
}

/// 绑定当前父调用的 single-use continuation
pub struct MiddlewareNext {
    host: HostClient,
}

impl MiddlewareNext {
    /// 消费 continuation 并进入链中下一层
    /// Rust 所有权阻止安全代码重复调用；
    /// Runtime 仍会拒绝恶意手写 RPC 的第二次调用
    pub async fn run(self, request: MiddlewareRequest) -> Result<MiddlewareResponse, PluginFault> {
        let (request, payload) = request.into_next_parts();
        let reply = self
            .host
            .call(
                NEXT_METHOD,
                serde_json::to_value(request).map_err(|_| invalid_input())?,
                payload,
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty() {
            return Err(invalid_input());
        }
        let response: MiddlewareNextResponse =
            serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        MiddlewareResponse::from_next(response, self.host)
    }
}

/// 作者返回的响应；完整 next 响应头以增量方式修改，未改写的字节保持原样
pub struct MiddlewareResponse {
    pub metadata: Option<Box<crate::call::model::facts::ProviderCallMetadata>>,
    pub protocol: String,
    pub status: u16,
    pub headers: Vec<MiddlewareHeader>,
    pub body: MiddlewareBody,
    source: Option<String>,
    original_protocol: Option<String>,
    original_status: Option<u16>,
    original_headers: Vec<MiddlewareHeader>,
    header_mutations: Vec<MiddlewareHeaderMutation>,
}

impl MiddlewareResponse {
    fn from_next(response: MiddlewareNextResponse, host: HostClient) -> Result<Self, PluginFault> {
        if response.response.is_empty()
            || response.protocol.is_empty()
            || !(100..=599).contains(&response.status)
            || response
                .body
                .as_ref()
                .is_some_and(|body| body.handle.is_empty())
        {
            return Err(invalid_input());
        }
        let body = response.body.map_or_else(MiddlewareBody::empty, |body| {
            MiddlewareBody::from_host(body, host)
        });
        Ok(Self {
            metadata: response.metadata,
            protocol: response.protocol.clone(),
            status: response.status,
            headers: response.headers.clone(),
            body,
            source: Some(response.response),
            original_protocol: Some(response.protocol),
            original_status: Some(response.status),
            original_headers: response.headers,
            header_mutations: Vec::new(),
        })
    }

    /// 创建不调用 next 的短路响应
    #[must_use]
    pub fn direct(
        protocol: impl Into<String>,
        status: u16,
        headers: Vec<MiddlewareHeader>,
        body: MiddlewareBody,
    ) -> Self {
        Self {
            metadata: None,
            protocol: protocol.into(),
            status,
            headers,
            body,
            source: None,
            original_protocol: None,
            original_status: None,
            original_headers: Vec::new(),
            header_mutations: Vec::new(),
        }
    }

    pub fn remove_header(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.headers
            .retain(|header| !header.name.eq_ignore_ascii_case(&name));
        self.header_mutations
            .push(MiddlewareHeaderMutation::Remove { name });
    }

    pub fn append_header(&mut self, name: impl Into<String>, value: Vec<u8>) {
        let name = name.into();
        self.headers.push(MiddlewareHeader {
            name: name.clone(),
            value: value.clone(),
        });
        self.header_mutations
            .push(MiddlewareHeaderMutation::Append { name, value });
    }

    /// 转为现有 RPC reply
    /// 即使正文 opaque 直通或为空也使用空 stream，确保单一 End
    pub fn into_reply(self) -> Result<CallReply, PluginFault> {
        if self.protocol.is_empty() || !(100..=599).contains(&self.status) {
            return Err(invalid_input());
        }
        let header_mutations =
            finish_header_mutations(&self.original_headers, self.header_mutations, self.headers);
        let protocol = (self.original_protocol.as_deref() != Some(self.protocol.as_str()))
            .then_some(self.protocol);
        let status = (self.original_status != Some(self.status)).then_some(self.status);
        let (body, stream) = self.body.into_wire()?;
        let result = serde_json::to_value(MiddlewareResponseHead {
            response: self.source,
            protocol,
            status,
            header_mutations,
            body,
        })
        .map_err(|_| invalid_input())?;
        Ok(CallReply::stream(result, Vec::new(), stream))
    }
}

/// 交给中间件作者的一次真实调用
pub struct RequestCall {
    pub context: CallContext,
    pub request: MiddlewareRequest,
    pub next: MiddlewareNext,
    pub cancellation: CallCancellation,
    pub host: HostClient,
}

impl RequestCall {
    /// 从 `middleware.handle` RPC 解码并交叉检查宿主签发的 mount/身份
    pub fn try_from(call: PluginCall) -> Result<Self, PluginFault> {
        if call.method != HANDLE_METHOD {
            return Err(PluginFault::new(
                ErrorCode::Unsupported,
                "plugin method is not supported",
            ));
        }
        let head: MiddlewareRequestHead =
            serde_json::from_value(call.params).map_err(|_| invalid_input())?;
        let mount = match call.context.stage {
            Stage::Request => MiddlewareMount::Request,
            Stage::Attempt => MiddlewareMount::Attempt,
            _ => return Err(invalid_input()),
        };
        if head.mount != mount
            || call.context.request_id.as_deref() != Some(head.request_id.as_str())
            || call.context.account_id != head.account_id
            || (mount == MiddlewareMount::Request && head.attempt_index.is_some())
            || (mount == MiddlewareMount::Attempt
                && head.attempt_index.is_none_or(|attempt| attempt == 0))
        {
            return Err(invalid_input());
        }
        let original_body = call.payload.clone();
        let original_protocol = head.protocol.clone();
        let original_headers = head.headers.clone();
        let host = call.host;
        Ok(Self {
            context: call.context,
            request: MiddlewareRequest {
                original_settings: head.settings.clone(),
                body: call.payload,
                original_body,
                body_replaced: false,
                original_protocol,
                original_headers,
                header_mutations: Vec::new(),
                capabilities: None,
                head,
            },
            next: MiddlewareNext { host: host.clone() },
            cancellation: call.cancellation,
            host,
        })
    }
}

fn invalid_input() -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, "middleware input is invalid")
}

/// 不同公开边界提供类型化视图，注册与组合仍使用同一个 middleware 函数
pub trait MiddlewareInput: Sized + Send + 'static {
    type Output: MiddlewareOutput;
    fn accepts(stage: Stage) -> bool;
    fn decode(call: PluginCall) -> Result<Self, PluginFault>;
}

pub trait MiddlewareOutput: Send + 'static {
    fn encode(self) -> Result<CallReply, PluginFault>;
}

impl MiddlewareInput for RequestCall {
    type Output = MiddlewareResponse;
    fn accepts(stage: Stage) -> bool {
        matches!(stage, Stage::Request | Stage::Attempt)
    }
    fn decode(call: PluginCall) -> Result<Self, PluginFault> {
        Self::try_from(call)
    }
}
impl MiddlewareOutput for MiddlewareResponse {
    fn encode(self) -> Result<CallReply, PluginFault> {
        self.into_reply()
    }
}
