//! OpenAI 原生请求的构造参数；只解释本次值，不维护插件身份策略

use gateway_core::{
    error::{ProviderError, ProviderErrorKind},
    upstream::UpstreamSendState,
};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
pub(super) struct RequestParameters {
    pub(super) installation_id: String,
}

impl RequestParameters {
    pub(super) fn baseline(installation_id: &str) -> Map<String, Value> {
        Map::from_iter([(
            "installation_id".into(),
            Value::String(installation_id.to_owned()),
        )])
    }

    pub(super) fn decode(parameters: Option<Map<String, Value>>) -> Result<Self, ProviderError> {
        let invalid = || {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            )
            .with_retry_prohibited()
        };
        // 参数基线已在选号后提供，显式删除和类型错误不能回退为账号默认身份
        serde_json::from_value(Value::Object(parameters.ok_or_else(invalid)?))
            .map_err(|_| invalid())
    }
}
