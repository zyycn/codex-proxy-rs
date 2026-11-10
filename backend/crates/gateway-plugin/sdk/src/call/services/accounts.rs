//! 账号自动重置的类型化操作与状态合同
//!
//! 从宿主状态类型与 public_service/accounts.rs 生成；更新命令见 SDK 维护说明
use super::Operation;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCreditAutomationStatus {
    pub enabled: bool,
    pub lead_minutes: u32,
    pub weekly_trigger_remaining_percent: Option<u32>,
    pub next_discovery_at: Option<String>,
    pub next_consume_at: Option<String>,
    pub last_result: Option<String>,
}
pub struct GetResetCreditAutomation;
impl Operation for GetResetCreditAutomation {
    const NAME: &'static str = "accounts.reset_credit_automation_status";
    type Input = String;
    type Output = ResetCreditAutomationStatus;
}
pub struct SetResetCreditAutomation;
impl Operation for SetResetCreditAutomation {
    const NAME: &'static str = "accounts.set_reset_credit_automation";
    type Input = (String, bool);
    type Output = ResetCreditAutomationStatus;
}
