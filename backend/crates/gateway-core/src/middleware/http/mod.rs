//! HTTP 中间件的值与流合同；不包含服务器、客户端或业务路由实现

pub mod upgrade;

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, combinators::UnsyncBoxBody};

use crate::{engine::middleware::MiddlewareError, lifecycle::CancellationToken};

/// 正文保留数据帧、trailers 和背压；不要求完整读取或重新编码
pub type Body = UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
pub type Request = http::Request<Body>;
pub type Response = http::Response<Body>;
pub type Next = super::Next<Request, Response, MiddlewareError>;

/// 路由前调用没有 Client Key；身份在默认认证流程中产生
#[derive(Clone, Debug)]
pub struct Context {
    pub request_id: String,
    pub plugin_instance_id: Option<String>,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub extensions: crate::engine::extensions::ExtensionCallScope,
    pub plan: Option<crate::engine::middleware::FrozenMiddlewarePlan>,
    pub cancellation: CancellationToken,
}

/// 内部子请求复用 API 总路由；端口不解释业务路径，也不经网络回环
pub trait Dispatcher: Send + Sync {
    fn request_settings(&self) -> Option<crate::settings::RequestSettings> {
        None
    }

    fn dispatch(
        &self,
        context: Context,
        request: Request,
    ) -> futures::future::BoxFuture<'static, Result<Response, MiddlewareError>>;
}

/// 宿主解析后放入请求 extensions，插件显式覆盖后由终端消费
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub timeout: Option<Duration>,
    pub runtime: Option<crate::settings::RequestSettings>,
}

#[must_use]
pub fn empty_body() -> Body {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync()
}
