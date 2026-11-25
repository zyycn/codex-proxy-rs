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
                .json(&json!({"project":project})),
            token,
        )
        .await
    }
}
