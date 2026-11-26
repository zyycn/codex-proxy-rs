//! Cloud Code 账号套餐、分组额度与模型额度的原生合同

use super::{CloudCodeClient, client::control, failure::sent};
use gateway_core::error::{ProviderError, ProviderErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountInfo {
    #[serde(rename = "cloudaicompanionProject")]
    pub(crate) project: String,
    pub(crate) paid_tier: Option<Tier>,
    pub(crate) current_tier: Option<Tier>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Tier {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(
        default,
        rename = "availableCredits",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub(crate) available_credits: Vec<Credit>,
}

pub(crate) const GOOGLE_ONE_AI: &str = "GOOGLE_ONE_AI";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Credit {
    #[serde(default)]
    credit_type: serde_json::Value,
    #[serde(default)]
    credit_amount: serde_json::Value,
    #[serde(default)]
    minimum_credit_amount_for_usage: serde_json::Value,
}

impl Credit {
    pub(crate) fn is_google_one(&self) -> bool {
        self.credit_type == GOOGLE_ONE_AI || self.credit_type.as_u64() == Some(1)
    }
    // protobuf 的 int64 通常编码成十进制字符串
    fn amount(value: &serde_json::Value) -> Option<u64> {
        let amount = value.as_i64().or_else(|| value.as_str()?.parse().ok())?;
        u64::try_from(amount).ok()
    }
    pub(crate) fn balance(&self) -> Option<u64> {
        Self::amount(&self.credit_amount)
    }
    pub(crate) fn usable(&self) -> bool {
        let minimum = if self.minimum_credit_amount_for_usage.is_null() {
            Some(0)
        } else {
            Self::amount(&self.minimum_credit_amount_for_usage)
        };
        self.balance()
            .zip(minimum)
            .is_some_and(|(balance, minimum)| balance > 0 && balance >= minimum)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelQuota {
    pub(crate) remaining_fraction: Option<serde_json::Number>,
    pub(crate) reset_time: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct QuotaSummary {
    pub(crate) groups: Vec<QuotaGroup>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuotaGroup {
    #[serde(skip_serializing)]
    pub(crate) description: Option<String>,
    pub(crate) display_name: String,
    pub(crate) buckets: Vec<QuotaBucket>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuotaBucket {
    pub(crate) bucket_id: String,
    pub(crate) display_name: String,
    pub(crate) window: String,
    #[serde(flatten)]
    pub(crate) quota: ModelQuota,
}

impl CloudCodeClient {
    pub(crate) async fn account_info(&self, token: &str) -> Result<AccountInfo, ProviderError> {
        let info: AccountInfo = control(
            self.post("loadCodeAssist", token)
                .timeout(std::time::Duration::from_secs(15))
                .json(&json!({"metadata":{}})),
            token,
        )
        .await?;
        let project = &info.project;
        if project.trim().is_empty() || project.len() > 255 || project.chars().any(char::is_control)
        {
            return Err(sent(ProviderErrorKind::Protocol));
        }
        Ok(info)
    }

    pub(crate) async fn quota_summary(
        &self,
        token: &str,
        project: &str,
    ) -> Result<QuotaSummary, ProviderError> {
        control(
            self.post("retrieveUserQuotaSummary", token)
                .timeout(std::time::Duration::from_secs(15))
                .json(&json!({"project":project})),
            token,
        )
        .await
    }
}
