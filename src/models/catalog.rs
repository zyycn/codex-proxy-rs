use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::ModelConfig;

const SERVICE_TIER_SUFFIXES: [&str; 2] = ["fast", "flex"];
const REASONING_EFFORT_SUFFIXES: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];
const STATIC_REASONING_EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortInfo {
    pub reasoning_effort: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexModelInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub is_default: bool,
    pub supported_reasoning_efforts: Vec<ReasoningEffortInfo>,
    pub default_reasoning_effort: String,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub supports_personality: bool,
    pub upgrade: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_policy_limit: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModelName {
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelStoreDebug {
    pub total_models: usize,
    pub static_models: usize,
    pub alias_count: usize,
    pub models: Vec<ModelDebugEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelDebugEntry {
    pub id: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    models: Vec<CodexModelInfo>,
    aliases: BTreeMap<String, String>,
    default_model: String,
    model_plan_index: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendReasoningEffort {
    #[serde(default, rename = "reasoning_effort", alias = "reasoningEffort")]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendTruncationPolicy {
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendModelEntry {
    pub slug: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub is_default: Option<bool>,
    pub default_reasoning_effort: Option<String>,
    pub default_reasoning_level: Option<String>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<BackendReasoningEffort>,
    #[serde(default)]
    pub supported_reasoning_levels: Vec<BackendReasoningEffort>,
    pub input_modalities: Option<Vec<String>>,
    pub output_modalities: Option<Vec<String>>,
    pub supports_personality: Option<bool>,
    pub upgrade: Option<String>,
    #[serde(alias = "contextWindow")]
    pub context_window: Option<u64>,
    #[serde(alias = "maxContextWindow")]
    pub max_context_window: Option<u64>,
    #[serde(alias = "maxOutputTokens")]
    pub max_output_tokens: Option<u64>,
    #[serde(alias = "truncationPolicy")]
    pub truncation_policy: Option<BackendTruncationPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPlanSnapshot {
    pub plan_type: String,
    pub models: Vec<CodexModelInfo>,
}

impl ModelPlanSnapshot {
    pub fn from_backend_entries(
        plan_type: impl Into<String>,
        entries: Vec<BackendModelEntry>,
    ) -> Self {
        Self {
            plan_type: plan_type.into(),
            models: entries.into_iter().map(normalize_backend_model).collect(),
        }
    }
}

impl ModelCatalog {
    pub fn from_config(config: &ModelConfig) -> Self {
        let default_model = config.default_model.trim().to_string();
        let aliases = normalize_aliases(&config.aliases);
        Self {
            models: vec![default_model_info(config)],
            aliases,
            default_model,
            model_plan_index: BTreeMap::new(),
        }
    }

    pub fn from_config_and_snapshots(
        config: &ModelConfig,
        snapshots: &[ModelPlanSnapshot],
    ) -> Self {
        if snapshots.is_empty() {
            return Self::from_config(config);
        }

        let aliases = normalize_aliases(&config.aliases);
        let default_model = config.default_model.trim().to_string();
        let mut models_by_id = BTreeMap::new();
        let mut model_plan_index: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for snapshot in snapshots {
            for model in &snapshot.models {
                // 后端模型是按账号 plan 返回的；catalog 需要去重展示，调度层仍要保留 model -> plans。
                models_by_id.insert(model.id.clone(), model.clone());
                let plans = model_plan_index.entry(model.id.clone()).or_default();
                if !plans.contains(&snapshot.plan_type) {
                    plans.push(snapshot.plan_type.clone());
                }
            }
        }

        let models = models_by_id.into_values().collect::<Vec<_>>();
        if models.is_empty() {
            return Self::from_config(config);
        }

        Self {
            models,
            aliases,
            default_model,
            model_plan_index,
        }
    }

    pub fn models(&self) -> Vec<CodexModelInfo> {
        self.models.clone()
    }

    pub fn model_info(&self, model_id: &str) -> Option<CodexModelInfo> {
        self.models
            .iter()
            .find(|model| model.id == model_id)
            .cloned()
    }

    pub fn debug(&self) -> ModelStoreDebug {
        ModelStoreDebug {
            total_models: self.models.len(),
            static_models: self.models.len(),
            alias_count: self.aliases.len(),
            models: self
                .models
                .iter()
                .map(|model| ModelDebugEntry {
                    id: model.id.clone(),
                    source: model.source.clone(),
                })
                .collect(),
        }
    }

    pub fn model_plan_allowlist(&self) -> BTreeMap<String, Vec<String>> {
        self.model_plan_index.clone()
    }

    pub fn is_recognized_model_name(&self, input: &str) -> bool {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return false;
        }
        if self.aliases.contains_key(trimmed) || self.model_info(trimmed).is_some() {
            return true;
        }

        let stripped = strip_known_model_suffixes(trimmed);
        if stripped.model_name == trimmed
            || (stripped.reasoning_effort.is_none() && stripped.service_tier.is_none())
        {
            return false;
        }
        self.aliases.contains_key(&stripped.model_name)
            || self.model_info(&stripped.model_name).is_some()
    }

    pub fn parse_model_name(&self, input: &str) -> ParsedModelName {
        let trimmed = input.trim();
        if self.aliases.contains_key(trimmed) || self.model_info(trimmed).is_some() {
            return ParsedModelName {
                model_id: self.resolve_model_id(trimmed),
                reasoning_effort: None,
                service_tier: None,
            };
        }

        let stripped = strip_known_model_suffixes(trimmed);
        ParsedModelName {
            model_id: self.resolve_model_id(&stripped.model_name),
            reasoning_effort: stripped.reasoning_effort,
            service_tier: stripped.service_tier,
        }
    }

    pub fn build_display_model_name(parsed: &ParsedModelName) -> String {
        let mut name = parsed.model_id.clone();
        if let Some(reasoning_effort) = &parsed.reasoning_effort {
            name.push('-');
            name.push_str(reasoning_effort);
        }
        if let Some(service_tier) = &parsed.service_tier {
            name.push('-');
            name.push_str(service_tier);
        }
        name
    }

    fn resolve_model_id(&self, input: &str) -> String {
        let resolved = self.resolve_alias_chain(input);
        if self.model_info(&resolved).is_some() {
            return resolved;
        }
        self.default_model.clone()
    }

    fn resolve_alias_chain(&self, input: &str) -> String {
        let original = input.trim();
        let mut current = original.to_string();
        let mut seen = BTreeSet::new();
        for _ in 0..20 {
            let Some(target) = self.aliases.get(&current).map(|value| value.trim()) else {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct StrippedModelName {
    model_name: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
}

fn strip_known_model_suffixes(input: &str) -> StrippedModelName {
    let mut remaining = input.trim().to_string();
    let service_tier = take_suffix(&mut remaining, &SERVICE_TIER_SUFFIXES);
    let reasoning_effort = take_suffix(&mut remaining, &REASONING_EFFORT_SUFFIXES);
    StrippedModelName {
        model_name: remaining,
        reasoning_effort,
        service_tier,
    }
}

fn take_suffix(remaining: &mut String, suffixes: &[&str]) -> Option<String> {
    let suffix = suffixes
        .iter()
        .find(|suffix| remaining.ends_with(&format!("-{suffix}")))?;
    let truncate_to = remaining.len() - suffix.len() - 1;
    remaining.truncate(truncate_to);
    Some((*suffix).to_string())
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

fn default_model_info(config: &ModelConfig) -> CodexModelInfo {
    let id = config.default_model.trim().to_string();
    let default_reasoning_effort = config
        .default_reasoning_effort
        .clone()
        .unwrap_or_else(|| "medium".to_string());
    CodexModelInfo {
        id: id.clone(),
        display_name: id,
        description: "Codex default model".to_string(),
        is_default: true,
        supported_reasoning_efforts: STATIC_REASONING_EFFORTS
            .into_iter()
            .map(|effort| ReasoningEffortInfo {
                reasoning_effort: effort.to_string(),
                description: effort.to_string(),
            })
            .collect(),
        default_reasoning_effort,
        input_modalities: vec!["text".to_string(), "image".to_string()],
        output_modalities: vec!["text".to_string()],
        supports_personality: false,
        upgrade: None,
        source: "static".to_string(),
        context_window: None,
        max_context_window: None,
        max_output_tokens: None,
        truncation_policy_limit: None,
    }
}

fn normalize_backend_model(raw: BackendModelEntry) -> CodexModelInfo {
    let id = first_non_empty([raw.slug.as_deref(), raw.id.as_deref(), raw.name.as_deref()])
        .unwrap_or("unknown")
        .to_string();
    let display_name = first_non_empty([raw.display_name.as_deref(), raw.name.as_deref()])
        .unwrap_or(&id)
        .to_string();
    let reasoning_entries = if raw.supported_reasoning_efforts.is_empty() {
        raw.supported_reasoning_levels
    } else {
        raw.supported_reasoning_efforts
    };
    let mut supported_reasoning_efforts = reasoning_entries
        .into_iter()
        .filter_map(normalize_backend_reasoning_effort)
        .collect::<Vec<_>>();
    if supported_reasoning_efforts.is_empty() {
        supported_reasoning_efforts.push(ReasoningEffortInfo {
            reasoning_effort: "medium".to_string(),
            description: "Default".to_string(),
        });
    }
    let default_reasoning_effort = first_non_empty([
        raw.default_reasoning_effort.as_deref(),
        raw.default_reasoning_level.as_deref(),
    ])
    .unwrap_or("medium")
    .to_string();

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
