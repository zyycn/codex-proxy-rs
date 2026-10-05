//! 编译插件绑定范围，并匹配身份、Provider、模型与调用场景

use std::collections::BTreeSet;

use gateway_admin::model::{AdminError, plugins::instances::PluginCapabilityBinding};
use gateway_core::{
    engine::{
        observation::{RequestObservation, WebSocketResponseObservation},
        policy::{AccountScheduleInput, ModelRouteInput},
    },
    identity::ProviderKind,
    policy::ClientApiKeyId,
    routing::{AccountGroupId, PublicModelId},
};

/// 由 Admin 已冻结引用编译出的通用请求绑定范围
///
/// 组条件采用“任一命中”；同一 Key 可以同时属于多个组，因此不同组集合不能单独证明两个
/// 调度绑定互斥
#[derive(Default)]
pub(crate) struct BindingScope {
    client_keys: BTreeSet<ClientApiKeyId>,
    account_groups: BTreeSet<AccountGroupId>,
    providers: BTreeSet<ProviderKind>,
    models: BTreeSet<String>,
}

impl BindingScope {
    pub(crate) fn compile(binding: &PluginCapabilityBinding) -> Result<Self, AdminError> {
        if binding.client_key_ids.len() > 256
            || binding.account_group_ids.len() > 256
            || binding.provider_ids.len() > 64
            || binding.models.len() > 256
        {
            return Err(AdminError::invalid("插件绑定范围过大"));
        }
        let client_keys: BTreeSet<ClientApiKeyId> = binding
            .client_key_ids
            .iter()
            .map(|key| {
                ClientApiKeyId::new(key.clone())
                    .map_err(|_| AdminError::invalid("插件绑定 Client Key 范围无效"))
            })
            .collect::<Result<_, _>>()?;
        let account_groups: BTreeSet<AccountGroupId> = binding
            .account_group_ids
            .iter()
            .map(|group| {
                AccountGroupId::new(group.clone())
                    .map_err(|_| AdminError::invalid("插件绑定账号组范围无效"))
            })
            .collect::<Result<_, _>>()?;
        let providers: BTreeSet<ProviderKind> = binding
            .provider_ids
            .iter()
            .map(|provider| {
                ProviderKind::new(provider.clone())
                    .map_err(|_| AdminError::invalid("插件绑定 Provider 范围无效"))
            })
            .collect::<Result<_, _>>()?;
        let models: BTreeSet<String> = binding
            .models
            .iter()
            .map(|model| {
                PublicModelId::new(model.clone())
                    .map(|model| model.as_str().to_owned())
                    .map_err(|_| AdminError::invalid("插件绑定模型范围无效"))
            })
            .collect::<Result<_, _>>()?;
        if client_keys.len() != binding.client_key_ids.len()
            || account_groups.len() != binding.account_group_ids.len()
            || providers.len() != binding.provider_ids.len()
            || models.len() != binding.models.len()
        {
            return Err(AdminError::invalid("插件绑定范围重复"));
        }
        Ok(Self {
            client_keys,
            account_groups,
            providers,
            models,
        })
    }

    pub(crate) fn has_provider_condition(&self) -> bool {
        !self.providers.is_empty()
    }

    pub(crate) fn matches_observation(&self, observation: &RequestObservation) -> bool {
        self.matches_identity(observation.client_key_id(), observation.account_group_ids())
            && self.matches_provider(observation.provider())
            && self.matches_model(observation.requested_model().map(PublicModelId::as_str))
    }

    pub(crate) fn matches_websocket_observation(
        &self,
        observation: &WebSocketResponseObservation,
    ) -> bool {
        self.matches_identity(observation.client_key_id(), observation.account_group_ids())
            && self.matches_provider(Some(observation.provider()))
            && self.matches_model(observation.requested_model().map(PublicModelId::as_str))
    }

    pub(crate) fn matches_route(&self, input: &ModelRouteInput) -> bool {
        // Router 执行时 Provider 尚未产生；带 Provider 条件的绑定在编译时拒绝
        !self.has_provider_condition()
            && self.matches_identity(input.client_key_id(), input.account_group_ids())
            && self.matches_model(Some(input.requested_model().as_str()))
    }

    pub(crate) fn matches_schedule(&self, input: &AccountScheduleInput) -> bool {
        self.matches_identity(input.client_key_id(), input.account_group_ids())
            && self.matches_provider(Some(input.provider()))
            && self.matches_model(input.model())
    }

    pub(crate) fn matches_processing(
        &self,
        client_key: &ClientApiKeyId,
        account_groups: &[AccountGroupId],
        provider: Option<&ProviderKind>,
        model: Option<&str>,
    ) -> bool {
        self.matches_identity(client_key, account_groups)
            && self.matches_provider(provider)
            && self.matches_model(model)
    }

    /// 保守判断两个范围能否同时匹配；只有 Key、Provider 或模型的明确不交集可证明互斥
    pub(crate) fn overlaps(&self, other: &Self) -> bool {
        !disjoint_when_both_constrained(&self.client_keys, &other.client_keys)
            && !disjoint_when_both_constrained(&self.providers, &other.providers)
            && !disjoint_when_both_constrained(&self.models, &other.models)
    }

    fn matches_identity(
        &self,
        client_key: &ClientApiKeyId,
        account_groups: &[AccountGroupId],
    ) -> bool {
        (self.client_keys.is_empty() || self.client_keys.contains(client_key))
            && (self.account_groups.is_empty()
                || account_groups
                    .iter()
                    .any(|group| self.account_groups.contains(group)))
    }

    fn matches_provider(&self, provider: Option<&ProviderKind>) -> bool {
        self.providers.is_empty()
            || provider.is_some_and(|provider| self.providers.contains(provider))
    }

    fn matches_model(&self, model: Option<&str>) -> bool {
        self.models.is_empty() || model.is_some_and(|model| self.models.contains(model))
    }
}

fn disjoint_when_both_constrained<T: Ord>(left: &BTreeSet<T>, right: &BTreeSet<T>) -> bool {
    !left.is_empty() && !right.is_empty() && left.is_disjoint(right)
}
