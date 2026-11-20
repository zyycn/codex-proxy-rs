//! 请求位置只描述出口的业务配置，不改变代理连接身份或服务系统时区

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequestLocation {
    pub country: String,
    pub region: String,
    pub city: String,
    pub timezone: chrono_tz::Tz,
}

/// 独立选择 OpenAI 请求的国家字段，仅自定义模式需要国家代码
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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
    pub fn validate(&self) -> Result<(), InvalidRequestLocation> {
        if let Self::Custom { country } = self {
            validate_country(country)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidRequestLocation {
    #[error("country must contain two uppercase ASCII letters")]
    Country,
    #[error("region must contain 1–128 characters without control characters")]
    Region,
    #[error("city must contain 1–128 characters without control characters")]
    City,
}

impl Default for RequestLocation {
    fn default() -> Self {
        Self {
            country: "US".to_owned(),
            region: "Ohio".to_owned(),
            city: "Piketon".to_owned(),
            timezone: chrono_tz::America::New_York,
        }
    }
}

impl RequestLocation {
    pub fn validate(&self) -> Result<(), InvalidRequestLocation> {
        validate_country(&self.country)?;
        for (value, error) in [
            (&self.region, InvalidRequestLocation::Region),
            (&self.city, InvalidRequestLocation::City),
        ] {
            // 先检查原始输入，避免 trim 掩盖首尾的换行和其他控制字符
            if value.chars().any(char::is_control)
                || value.trim().is_empty()
                || value.trim().chars().count() > 128
            {
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn normalized(mut self) -> Result<Self, InvalidRequestLocation> {
        self.validate()?;
        self.region = self.region.trim().to_owned();
        self.city = self.city.trim().to_owned();
        Ok(self)
    }
}

fn validate_country(country: &str) -> Result<(), InvalidRequestLocation> {
    if country.len() == 2 && country.bytes().all(|byte| byte.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(InvalidRequestLocation::Country)
    }
}
