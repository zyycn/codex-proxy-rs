//! 路由前 HTTP 视图；复用 middleware.handle、next 与正文资源调用

use super::MiddlewareHeader;
use serde::{Deserialize, Serialize};

/// 正文句柄消费一次；Bytes 使用当前 RPC 的 binary payload，Stream 使用响应流
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Body {
    Empty,
    Handle { handle: String },
    Bytes,
    Stream,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// 认证与解压前的运行设置；null 表示主动调用继承宿主或父调用基线
    pub settings: serde_json::Value,
    pub method: String,
    pub uri: String,
    pub version: Version,
    pub headers: Vec<MiddlewareHeader>,
    /// 缺省超时由宿主形成基线；None 显式取消本次 HTTP 处理期限
    pub timeout_ms: Option<u64>,
    pub body: Body,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub status: u16,
    pub version: Version,
    pub headers: Vec<MiddlewareHeader>,
    pub body: Body,
    /// 返还 next 的原响应时保留该关联，使宿主继续持有传输扩展和资源
    pub response: Option<String>,
    /// 响应流承载会话处理的完成与错误，不承载 HTTP 正文
    pub session: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Version {
    #[serde(rename = "HTTP/0.9")]
    Http09,
    #[serde(rename = "HTTP/1.0")]
    Http10,
    #[serde(rename = "HTTP/1.1")]
    Http11,
    #[serde(rename = "HTTP/2")]
    Http2,
    #[serde(rename = "HTTP/3")]
    Http3,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub settings_sources: serde_json::Value,
    pub request_id: String,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub request: Request,
}

/// 数据在 binary payload，trailers 保留多值 header；EOF 不携带正文或 trailers
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyRead {
    pub eof: bool,
    pub trailers: Option<Vec<MiddlewareHeader>>,
}

pub const DISPATCH_METHOD: &str = "host.http.dispatch";
pub const BODY_READ_METHOD: &str = "host.http.body_read";
pub const BODY_CLOSE_METHOD: &str = "host.http.body_close";
pub const BODY_CREATE_METHOD: &str = "host.http.body_create";
pub const BODY_WRITE_METHOD: &str = "host.http.body_write";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyPipe {
    pub handle: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyWrite {
    pub handle: String,
    pub eof: bool,
    pub trailers: Option<Vec<MiddlewareHeader>>,
}

pub const UPGRADE_METHOD: &str = "host.middleware.upgrade";
pub const RECEIVE_METHOD: &str = "host.middleware.receive";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upgrade {
    pub request: Request,
    pub protocols: Vec<String>,
}
