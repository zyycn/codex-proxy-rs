//! 冻结宿主基线与请求显式改写；派生快照只属于当前调用，不发布全局配置

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::policy::{ClientApiKeyId, ClientPolicy, ClientSettings, RateLimits};
use crate::routing::RuntimeSnapshot;

pub(crate) mod compiled;
mod values;
pub use values::SettingsValues;

/// 一次模型调用的有效设置；改写只影响当前请求，不发布配置或修改持久化 revision
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSettings {
    pub runtime: SettingsValues,
    pub disable_fast: bool,
    pub client_limits: RateLimits,
    /// null 表示不限制总时长；显式时限从请求开始计时，改写不重置计时原点
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingOverride<T = Value> {
    pub instance_id: String,
    pub order: u64,
    pub value: T,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ExecutionOverrides {
    #[serde(skip)]
    defaults: Arc<ClientSettings>,
    client_key_id: String,
    input: Arc<ExecutionSettings>,
    disable_fast: Option<SettingOverride<bool>>,
    client_limits: Option<SettingOverride<RateLimits>>,
    timeout_ms: Option<SettingOverride<Option<u64>>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct HttpTimeout {
    input_ms: Option<u64>,
    change: Option<SettingOverride<Option<u64>>>,
}

/// 共享值均不可变；同级子调用的修改不会回写父调用
#[derive(Clone, Debug)]
pub struct RequestSettings {
    baseline: Arc<RuntimeSnapshot>,
    effective: Arc<RuntimeSnapshot>,
    overrides: Arc<BTreeMap<String, SettingOverride>>,
    execution: Option<Arc<ExecutionOverrides>>,
    http_timeout: Option<HttpTimeout>,
    order: u64,
}

impl PartialEq for RequestSettings {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.baseline, &other.baseline)
            && self.overrides == other.overrides
            && self.execution == other.execution
            && self.http_timeout == other.http_timeout
    }
}
impl Eq for RequestSettings {}

#[derive(Debug, thiserror::Error)]
#[error("request settings are invalid")]
pub struct InvalidSettings;

impl RequestSettings {
    #[must_use]
    pub fn new(snapshot: Arc<RuntimeSnapshot>) -> Self {
        Self {
            baseline: snapshot.clone(),
            effective: snapshot,
            overrides: Arc::default(),
            execution: None,
            http_timeout: None,
            order: 0,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Arc<RuntimeSnapshot> {
        self.effective.clone()
    }

    #[must_use]
    pub fn values(&self) -> &SettingsValues {
        self.effective.settings()
    }

    /// 来源与有效值分开查询，不允许插件伪造其他实例的写入来源
    pub fn inspect(&self) -> Value {
        serde_json::json!({
            "config_revision": self.baseline.revision().get(),
            "host": self.baseline.settings(),
            "overrides": self.overrides,
            "execution": self.execution,
            "http_timeout": self.http_timeout,
        })
    }

    pub fn replace(
        &self,
        values: SettingsValues,
        instance_id: &str,
    ) -> Result<Self, InvalidSettings> {
        self.replace_scoped(self.values(), values, instance_id)
    }

    /// Key 画像等隐式作用域结果不记录成插件改写，避免随后切换 Key 时泄漏
    fn replace_scoped(
        &self,
        previous: &SettingsValues,
        values: SettingsValues,
        instance_id: &str,
    ) -> Result<Self, InvalidSettings> {
        if previous == &values {
            return Ok(self.clone());
        }
        let current = serde_json::to_value(previous).map_err(|_| InvalidSettings)?;
        let next = serde_json::to_value(&values).map_err(|_| InvalidSettings)?;
        let order = self.order.checked_add(1).ok_or(InvalidSettings)?;
        let mut overrides = self.overrides.as_ref().clone();
        let effective_values = self.values();
        // 每个已声明设置项整体替换，不猜测画像对象、数组或 null 的深合并含义
        for (name, value) in next.as_object().ok_or(InvalidSettings)? {
            if current.get(name) != Some(value) {
                overrides.insert(
                    name.clone(),
                    SettingOverride {
                        instance_id: instance_id.to_owned(),
                        order,
                        value: value.clone(),
                    },
                );
            }
        }
        // 只把本层实际修改的字段写回宿主层，作用域默认值不参与继承
        let values = if previous == effective_values {
            values
        } else {
            apply_overrides(self.baseline.settings(), &overrides)?
        };
        let effective = self
            .effective
            .resolve_settings(&values)
            .map_err(|_| InvalidSettings)?;
        Ok(Self {
            baseline: self.baseline.clone(),
            effective,
            overrides: Arc::new(overrides),
            execution: self.execution.clone(),
            http_timeout: self.http_timeout.clone(),
            order,
        })
    }

    /// 持续会话的新请求读取当前宿主快照，只继承明确改写的字段
    pub fn rebase(&self, snapshot: Arc<RuntimeSnapshot>) -> Result<Self, InvalidSettings> {
        if Arc::ptr_eq(&self.baseline, &snapshot) {
            return Ok(self.clone());
        }
        let effective = if self.overrides.is_empty() {
            snapshot.clone()
        } else {
            snapshot
                .resolve_settings(&apply_overrides(snapshot.settings(), &self.overrides)?)
                .map_err(|_| InvalidSettings)?
        };
        Ok(Self {
            baseline: snapshot,
            effective,
            overrides: self.overrides.clone(),
            execution: self.execution.clone(),
            http_timeout: self.http_timeout.clone(),
            order: self.order,
        })
    }

    /// 认证、模型入口和插件视图共用解析规则，派生策略不改变 Key 默认值
    #[must_use]
    pub fn apply_policy(&self, policy: ClientPolicy) -> ClientPolicy {
        let values = self.resolve_execution(policy.key_id().as_str(), policy.defaults(), None);
        policy.with_settings(
            values.runtime.request_profiles(),
            values.disable_fast,
            values.client_limits,
        )
    }

    #[must_use]
    pub fn with_execution(mut self, policy: &ClientPolicy, timeout_ms: Option<u64>) -> Self {
        let input = self.resolve_execution(policy.key_id().as_str(), policy.defaults(), timeout_ms);
        let previous = self
            .execution
            .as_ref()
            .filter(|scope| scope.client_key_id == policy.key_id().as_str());
        self.execution = Some(Arc::new(ExecutionOverrides {
            defaults: policy.defaults().clone(),
            client_key_id: policy.key_id().as_str().to_owned(),
            input: Arc::new(input),
            disable_fast: previous.and_then(|scope| scope.disable_fast.clone()),
            client_limits: previous.and_then(|scope| scope.client_limits.clone()),
            timeout_ms: previous.and_then(|scope| scope.timeout_ms.clone()),
        }));
        self
    }

    #[must_use]
    pub fn execution_values(&self) -> Option<ExecutionSettings> {
        let scope = self.execution.as_ref()?;
        Some(self.resolve_execution(
            &scope.client_key_id,
            &scope.defaults,
            scope.input.timeout_ms,
        ))
    }

    fn resolve_execution(
        &self,
        key: &str,
        defaults: &ClientSettings,
        timeout_ms: Option<u64>,
    ) -> ExecutionSettings {
        let overrides = self
            .execution
            .as_ref()
            .filter(|scope| scope.client_key_id == key);
        let mut runtime = self.values().clone();
        // Key 画像覆盖宿主默认，插件显式覆盖整个设置项；此优先级只在这里解释
        if !self.overrides.contains_key("request_profiles") && !defaults.request_profiles.is_empty()
        {
            let mut profiles = runtime.request_profiles().clone();
            profiles.extend(defaults.request_profiles.clone());
            runtime = runtime.with_request_profiles(profiles);
        }
        ExecutionSettings {
            runtime,
            disable_fast: overrides
                .and_then(|scope| scope.disable_fast.as_ref())
                .map_or(defaults.disable_fast, |change| change.value),
            client_limits: overrides
                .and_then(|scope| scope.client_limits.as_ref())
                .map_or(defaults.limits, |change| change.value),
            timeout_ms: overrides
                .and_then(|scope| scope.timeout_ms.as_ref())
                .map_or(timeout_ms, |change| change.value),
        }
    }

    pub(crate) fn execution_deadline(
        &self,
        key: &ClientApiKeyId,
        started_at: SystemTime,
    ) -> Result<crate::lifecycle::Deadline, InvalidSettings> {
        let scope = self
            .execution
            .as_ref()
            .filter(|scope| scope.client_key_id == key.as_str())
            .ok_or(InvalidSettings)?;
        let timeout_ms = scope
            .timeout_ms
            .as_ref()
            .map_or(scope.input.timeout_ms, |change| change.value);
        crate::lifecycle::Deadline::from_timeout(started_at, timeout_ms.map(Duration::from_millis))
            .ok_or(InvalidSettings)
    }

    pub fn replace_execution(
        &self,
        values: &ExecutionSettings,
        instance_id: &str,
    ) -> Result<Self, InvalidSettings> {
        let previous = self.execution_values().ok_or(InvalidSettings)?;
        let mut updated =
            self.replace_scoped(&previous.runtime, values.runtime.clone(), instance_id)?;
        if previous.disable_fast == values.disable_fast
            && previous.client_limits == values.client_limits
            && previous.timeout_ms == values.timeout_ms
        {
            return Ok(updated);
        }
        let mut scope = self.execution.as_deref().cloned().ok_or(InvalidSettings)?;
        let order = if updated.order == self.order {
            self.order.checked_add(1).ok_or(InvalidSettings)?
        } else {
            updated.order
        };
        if previous.disable_fast != values.disable_fast {
            scope.disable_fast = Some(SettingOverride {
                instance_id: instance_id.to_owned(),
                order,
                value: values.disable_fast,
            });
        }
        if previous.client_limits != values.client_limits {
            scope.client_limits = Some(SettingOverride {
                instance_id: instance_id.to_owned(),
                order,
                value: values.client_limits,
            });
        }
        if previous.timeout_ms != values.timeout_ms {
            scope.timeout_ms = Some(SettingOverride {
                instance_id: instance_id.to_owned(),
                order,
                value: values.timeout_ms,
            });
        }
        updated.execution = Some(Arc::new(scope));
        updated.order = order;
        Ok(updated)
    }

    #[must_use]
    pub fn execution_timeout(&self, key: &ClientApiKeyId) -> Option<Duration> {
        self.execution
            .as_ref()
            .filter(|scope| scope.client_key_id == key.as_str())
            .and_then(|scope| scope.timeout_ms.as_ref())
            .and_then(|change| change.value.map(Duration::from_millis))
    }

    #[must_use]
    pub fn with_http_timeout(mut self, timeout_ms: Option<u64>) -> Self {
        self.http_timeout.get_or_insert(HttpTimeout {
            input_ms: timeout_ms,
            change: None,
        });
        self
    }

    pub fn replace_http_timeout(
        mut self,
        timeout_ms: Option<u64>,
        instance_id: &str,
    ) -> Result<Self, InvalidSettings> {
        let timeout = self.http_timeout.get_or_insert(HttpTimeout {
            input_ms: None,
            change: None,
        });
        if timeout
            .change
            .as_ref()
            .map_or(timeout.input_ms, |change| change.value)
            != timeout_ms
        {
            self.order = self.order.checked_add(1).ok_or(InvalidSettings)?;
            timeout.change = Some(SettingOverride {
                instance_id: instance_id.to_owned(),
                order: self.order,
                value: timeout_ms,
            });
        }
        Ok(self)
    }
}

fn apply_overrides(
    values: &SettingsValues,
    overrides: &BTreeMap<String, SettingOverride>,
) -> Result<SettingsValues, InvalidSettings> {
    let mut values = serde_json::to_value(values).map_err(|_| InvalidSettings)?;
    let object = values.as_object_mut().ok_or(InvalidSettings)?;
    for (name, change) in overrides {
        object.insert(name.clone(), change.value.clone());
    }
    serde_json::from_value(values).map_err(|_| InvalidSettings)
}
