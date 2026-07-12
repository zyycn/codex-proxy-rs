//! 模型目录聚合与模型名解析。

use std::collections::{BTreeMap, BTreeSet};

use super::types::{
    BackendModelEntry, BackendReasoningEffort, CodexModelInfo, ModelConfig, ModelPlanSnapshot,
    ReasoningEffortInfo,
};

/// 模型目录聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    models: Vec<CodexModelInfo>,
    model_aliases: BTreeMap<String, String>,
    model_plan_index: BTreeMap<String, Vec<String>>,
    fetched_plan_types: BTreeSet<String>,
}

impl ModelPlanSnapshot {
    /// 将上游模型条目标准化为可持久化快照。
    pub fn from_backend_entries(
        plan_type: impl Into<String>,
        entries: Vec<BackendModelEntry>,
    ) -> Self {
        Self {
            plan_type: plan_type.into(),
            models: entries.into_iter().map(normalize_backend_model).collect(),
        }
    }

    /// 将上游 JSON 条目解析并标准化为可持久化快照。
    pub fn from_backend_values(
        plan_type: impl Into<String>,
        entries: Vec<serde_json::Value>,
    ) -> Self {
        Self::from_backend_entries(
            plan_type,
            entries
                .into_iter()
                .filter_map(|entry| serde_json::from_value(entry).ok())
                .collect(),
        )
    }
}

impl ModelCatalog {
    /// 从运行时别名构造尚未刷新上游快照的空模型目录。
    pub fn from_config(config: &ModelConfig) -> Self {
        let model_aliases = normalize_aliases(&config.model_aliases);
        Self {
            models: Vec::new(),
            model_aliases,
            model_plan_index: BTreeMap::new(),
            fetched_plan_types: BTreeSet::new(),
        }
    }

    /// 从静态配置和后端快照构造模型目录。
    pub fn from_config_and_snapshots(
        config: &ModelConfig,
        snapshots: &[ModelPlanSnapshot],
    ) -> Self {
        if snapshots.is_empty() {
            return Self::from_config(config);
        }

        let model_aliases = normalize_aliases(&config.model_aliases);
        let mut models_by_id = BTreeMap::new();
        let mut model_order = Vec::new();
        let mut model_plan_index: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut fetched_plan_types = BTreeSet::new();

        for snapshot in snapshots {
            fetched_plan_types.insert(snapshot.plan_type.clone());
            for model in &snapshot.models {
                if !models_by_id.contains_key(&model.id) {
                    model_order.push(model.id.clone());
                }
                models_by_id.insert(model.id.clone(), model.clone());
                let plans = model_plan_index.entry(model.id.clone()).or_default();
                if !plans.contains(&snapshot.plan_type) {
                    plans.push(snapshot.plan_type.clone());
                }
            }
        }

        let models = model_order
            .into_iter()
            .filter_map(|id| models_by_id.remove(&id))
            .collect::<Vec<_>>();
        if models.is_empty() {
            return Self {
                models,
                model_aliases,
                model_plan_index,
                fetched_plan_types,
            };
        }

        Self {
            models,
            model_aliases,
            model_plan_index,
            fetched_plan_types,
        }
    }

    /// 返回对外可见的模型列表副本。
    pub fn models(&self) -> Vec<CodexModelInfo> {
        self.models.clone()
    }

    /// 返回 OpenAI `/v1/models` 对外暴露的模型 ID。
    pub fn public_model_ids(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut ids = Vec::new();
        for model in &self.models {
            if seen.insert(model.id.clone()) {
                ids.push(model.id.clone());
            }
        }
        for alias in self.model_aliases.keys() {
            if seen.insert(alias.clone()) {
                ids.push(alias.clone());
            }
        }
        ids
    }

    /// 返回模型数量。
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// 模型目录是否为空。
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// 返回指定订阅计划可用的模型列表。
    pub fn models_for_plan(&self, plan_type: &str) -> Vec<CodexModelInfo> {
        self.models
            .iter()
            .filter(|model| {
                self.model_plan_index
                    .get(&model.id)
                    .is_some_and(|plans| plans.iter().any(|plan| plan == plan_type))
            })
            .cloned()
            .collect()
    }

    /// 按模型 ID 查询模型信息。
    pub fn model_info(&self, model_id: &str) -> Option<CodexModelInfo> {
        self.models
            .iter()
            .find(|model| model.id == model_id)
            .cloned()
    }

    /// 按对外模型名查询模型信息，支持别名映射。
    pub fn model_info_for_name(&self, input: &str) -> Option<CodexModelInfo> {
        self.model_info(&self.resolve_model_id(input))
    }

    /// 返回 model -> plans allowlist。
    pub fn model_plan_allowlist(&self) -> BTreeMap<String, Vec<String>> {
        self.model_plan_index.clone()
    }

    /// 返回已成功拉取过模型列表的订阅计划。
    pub fn fetched_plan_types(&self) -> BTreeSet<String> {
        self.fetched_plan_types.clone()
    }

    /// 判断输入模型名是否在目录中可识别。
    pub fn is_recognized_model_name(&self, input: &str) -> bool {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return false;
        }
        self.model_aliases.contains_key(trimmed) || self.model_info(trimmed).is_some()
    }

    /// 沿 alias 链将外部模型名解析为真实 model ID。
    ///
    /// 模型名只承载 model 身份；reasoning effort、service tier 等运行参数由请求
    /// body 携带并原样透传上游。
    pub fn resolve_model_id(&self, input: &str) -> String {
        self.resolve_alias_chain(input)
    }

    fn resolve_alias_chain(&self, input: &str) -> String {
        let original = input.trim();
        let mut current = original.to_string();
        let mut seen = BTreeSet::new();
        for _ in 0..20 {
            let Some(target) = self.model_aliases.get(&current).map(|value| value.trim()) else {
                return current;
            };
            if !seen.insert(current.clone()) || seen.contains(target) {
                return original.to_string();
            }
            current = target.to_string();
        }
        original.to_string()
    }
}

fn normalize_aliases(input: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    input
        .iter()
        .filter_map(|(alias, target)| {
            let alias = alias.trim();
            let target = target.trim();
            (!alias.is_empty() && !target.is_empty())
                .then(|| (alias.to_string(), target.to_string()))
        })
        .collect()
}

fn normalize_backend_model(raw: BackendModelEntry) -> CodexModelInfo {
    let id = first_non_empty([raw.slug.as_deref(), raw.id.as_deref(), raw.name.as_deref()])
        .unwrap_or("unknown")
        .to_string();
    let display_name = first_non_empty([
        raw.display_name.as_deref(),
        raw.title.as_deref(),
        raw.name.as_deref(),
    ])
    .unwrap_or(&id)
    .to_string();
    let reasoning_entries = if raw.supported_reasoning_efforts.is_empty() {
        raw.supported_reasoning_levels
    } else {
        raw.supported_reasoning_efforts
    };
    let supported_reasoning_efforts = reasoning_entries
        .into_iter()
        .filter_map(normalize_backend_reasoning_effort)
        .collect::<Vec<_>>();
    let default_reasoning_effort = first_non_empty([
        raw.default_reasoning_effort.as_deref(),
        raw.default_reasoning_level.as_deref(),
    ])
    .map(ToString::to_string)
    .or_else(|| {
        supported_reasoning_efforts
            .first()
            .map(|effort| effort.reasoning_effort.clone())
    })
    .unwrap_or_default();

    CodexModelInfo {
        id,
        display_name,
        description: raw.description.unwrap_or_default(),
        is_default: raw.is_default.unwrap_or(false),
        supported_reasoning_efforts,
        default_reasoning_effort,
        input_modalities: raw
            .input_modalities
            .unwrap_or_else(|| vec!["text".to_string()]),
        output_modalities: raw
            .output_modalities
            .unwrap_or_else(|| vec!["text".to_string()]),
        supports_personality: raw.supports_personality.unwrap_or(false),
        upgrade: raw.upgrade,
        source: "backend".to_string(),
        context_window: raw.context_window,
        max_context_window: raw.max_context_window,
        max_output_tokens: raw.max_output_tokens,
        truncation_policy_limit: raw.truncation_policy.and_then(|policy| policy.limit),
    }
}

fn normalize_backend_reasoning_effort(raw: BackendReasoningEffort) -> Option<ReasoningEffortInfo> {
    let reasoning_effort =
        first_non_empty([raw.reasoning_effort.as_deref(), raw.effort.as_deref()])?.to_string();
    Some(ReasoningEffortInfo {
        description: raw.description.unwrap_or_default(),
        reasoning_effort,
    })
}

fn first_non_empty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
}
