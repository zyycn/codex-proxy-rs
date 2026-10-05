//! 公开服务的类型化操作合同；名称用于分派，不承担授权

pub mod settings;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const CALL_METHOD: &str = "host.services.call";

pub trait Operation {
    const NAME: &'static str;
    type Input: Serialize + DeserializeOwned + Send + 'static;
    type Output: Serialize + DeserializeOwned + Send + 'static;
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceError {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for ServiceError {}

pub type Response = Result<serde_json::Value, ServiceError>;

/// 洋葱层保留父子调用标识；input 由 Operation 对应的类型解释
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub operation: String,
    pub call_id: String,
    pub parent_call_id: Option<String>,
    pub request_id: String,
    pub input: serde_json::Value,
}
