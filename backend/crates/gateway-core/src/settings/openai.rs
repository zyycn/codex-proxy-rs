//! OpenAI 请求的国家、路由头与 OAuth 审核模型设置

/// OpenAI 请求策略与覆盖值，独立于客户端身份和并发调度
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenAiRequestSettings {
    pub request_country: OpenAiRequestCountry,
    pub oauth_guardian_review_model: bool,
    pub residency: Option<String>,
    pub account_routing_override: Option<String>,
}

impl OpenAiRequestSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.request_country
            .validate()
            .map_err(|_| "openai_request_settings.request_country")?;
        for (field, value) in [
            (
                "openai_request_settings.residency",
                self.residency.as_deref(),
            ),
            (
                "openai_request_settings.account_routing_override",
                self.account_routing_override.as_deref(),
            ),
        ] {
            let Some(value) = value else { continue };
            // WebSocket 握手也通过字符串构造头，提前拒绝无法在两种传输间一致发送的值
            if !http::HeaderValue::from_str(value).is_ok_and(|header| header.to_str().is_ok()) {
                return Err(field);
            }
        }
        Ok(())
    }
}

impl Default for OpenAiRequestSettings {
    fn default() -> Self {
        Self {
            request_country: Default::default(),
            oauth_guardian_review_model: true,
            residency: None,
            account_routing_override: None,
        }
    }
}

/// 独立选择 OpenAI 请求的国家字段，仅自定义模式需要国家代码
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpenAiRequestCountry {
    Default {},
    Passthrough {},
    FollowLocation {},
    Custom { country: String },
}

impl Default for OpenAiRequestCountry {
    fn default() -> Self {
        Self::FollowLocation {}
    }
}

impl OpenAiRequestCountry {
    pub fn validate(&self) -> Result<(), crate::account::InvalidRequestLocation> {
        if let Self::Custom { country } = self {
            crate::account::validate_country(country)?;
        }
        Ok(())
    }
}
