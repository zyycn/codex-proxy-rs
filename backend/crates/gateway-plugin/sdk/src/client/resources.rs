use crate::{PluginFault, call::resources};

use super::{HostClient, payload_call};

impl HostClient {
    /// 幂等创建或读取本实例分组，不覆盖管理员修改的现有属性。
    ///
    /// # Errors
    /// 权限、实例版本或输入不合法，或者宿主写入失败时返回错误。
    pub async fn ensure_group(
        &self,
        request: resources::GroupEnsureRequest,
    ) -> Result<resources::ManagedResource, PluginFault> {
        payload_call(self, resources::GROUP_ENSURE, request).await
    }

    /// 增量修改本实例分组的成员；保留账号在其他分组中的成员关系。
    ///
    /// # Errors
    /// 权限、实例版本、资源归属或输入不合法，或者宿主写入失败时返回错误。
    pub async fn change_group_members(
        &self,
        request: resources::GroupMembersChange,
    ) -> Result<resources::GroupMembersChanged, PluginFault> {
        payload_call(self, resources::GROUP_MEMBERS, request).await
    }

    /// 幂等创建或读取仅绑定本实例分组的 Key；不返回密钥明文。
    ///
    /// # Errors
    /// 权限、实例版本、资源归属或输入不合法，或者宿主写入失败时返回错误。
    pub async fn ensure_key(
        &self,
        request: resources::KeyEnsureRequest,
    ) -> Result<resources::ManagedResource, PluginFault> {
        payload_call(self, resources::KEY_ENSURE, request).await
    }
}
