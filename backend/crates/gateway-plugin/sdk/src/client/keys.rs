//! 非秘密 Key 目录与预算管理的类型化调用

use crate::{
    ErrorCode, PluginFault,
    call::{
        host::{KeyListRequest, KeyListResult},
        key_budgets,
    },
};

use super::{HostClient, SessionError, payload_call};

impl HostClient {
    /// 只读查询单个 Key 的预算，不开启或重置窗口
    ///
    /// # Errors
    /// Key 不存在或宿主读取失败时返回错误
    pub async fn get_key_budget(
        &self,
        request: key_budgets::GetKeyBudgetRequest,
    ) -> Result<key_budgets::KeyBudget, PluginFault> {
        payload_call(self, key_budgets::GET, request).await
    }

    /// 更新指定日／周上限；省略项不变，零表示不限，不清零用量
    ///
    /// # Errors
    /// 参数无效、Key 不存在或宿主写入失败时返回错误
    pub async fn update_key_budget_limits(
        &self,
        request: key_budgets::UpdateKeyBudgetLimitsRequest,
    ) -> Result<key_budgets::UpdateKeyBudgetLimitsResult, PluginFault> {
        payload_call(self, key_budgets::UPDATE_LIMITS, request).await
    }

    /// 查询 Key 的非秘密身份；调用与父资源保持关联
    ///
    /// # Errors
    /// 分页参数不合法，或者宿主读取失败时返回错误
    pub async fn list_keys(&self, query: KeyListRequest) -> Result<KeyListResult, PluginFault> {
        let invalid = || PluginFault::new(ErrorCode::InvalidInput, "invalid key list payload");
        let reply = self
            .call(
                "host.keys.list",
                serde_json::to_value(query).map_err(|_| invalid())?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty() {
            return Err(invalid());
        }
        serde_json::from_value(reply.result).map_err(|_| invalid())
    }

    /// 清零并关闭指定周期，保留限额，下次使用时重新开启；结果未知时不能盲目重试
    ///
    /// # Errors
    /// 实例过期、Key 不存在、宿主写入失败时返回错误
    pub async fn reset_key_budget(
        &self,
        request: key_budgets::ResetKeyBudgetRequest,
    ) -> Result<key_budgets::ResetKeyBudgetResult, PluginFault> {
        payload_call(self, key_budgets::RESET, request).await
    }
}
