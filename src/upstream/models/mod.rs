//! 模型目录领域：数据类型、快照存储端口与服务。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use thiserror::Error;

use crate::upstream::accounts::{
    model::{Account, AccountStatus},
    store::AccountStore,
};

// ---------------------------------------------------------------------------
// 数据类型
// ---------------------------------------------------------------------------

/// 模型目录别名配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelConfig {
    /// 模型别名映射。
    pub model_aliases: BTreeMap<String, String>,
}

/// 单个 reasoning effort 的标准化展示信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortInfo {
    /// 推理强度名。
    pub reasoning_effort: String,
    /// 展示描述。
    pub description: String,
}

/// 对外暴露的单个模型目录条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexModelInfo {
    /// 模型唯一 ID。
    pub id: String,
    /// 展示名。
    pub display_name: String,
    /// 描述文本。
    pub description: String,
    /// 是否默认模型。
    pub is_default: bool,
    /// 支持的推理强度列表。
    pub supported_reasoning_efforts: Vec<ReasoningEffortInfo>,
    /// 默认推理强度。
    pub default_reasoning_effort: String,
    /// 输入模态。
    pub input_modalities: Vec<String>,
    /// 输出模态。
    pub output_modalities: Vec<String>,
    /// 是否支持 personality。
    pub supports_personality: bool,
    /// 升级提示。
    pub upgrade: Option<String>,
    /// 来源标记。
    pub source: String,
    /// 当前上下文窗口。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 最大上下文窗口。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_window: Option<u64>,
    /// 最大输出 token 数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// 截断策略限制。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_policy_limit: Option<u64>,
}

/// 解析模型名后得到的标准化结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModelName {
    /// 最终模型 ID。
    pub model_id: String,
    /// 推理强度后缀。
    pub reasoning_effort: Option<String>,
    /// 服务层级后缀。
    pub service_tier: Option<String>,
}

/// 按计划类型持久化的模型快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPlanSnapshot {
    /// 订阅计划类型。
    pub plan_type: String,
    /// 该计划可见的模型列表。
    pub models: Vec<CodexModelInfo>,
}

/// 上游声明的 reasoning effort 条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendReasoningEffort {
    /// reasoning_effort 字段。
    #[serde(default, rename = "reasoning_effort", alias = "reasoningEffort")]
    pub reasoning_effort: Option<String>,
    /// 上游可能返回的 effort 字段别名。
    #[serde(default)]
    pub effort: Option<String>,
    /// 展示描述。
    #[serde(default)]
    pub description: Option<String>,
}

/// 上游截断策略定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendTruncationPolicy {
    /// 上下文截断限制。
    pub limit: Option<u64>,
}

/// 上游 `/codex/models` 返回的原始模型条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendModelEntry {
    /// slug。
    pub slug: Option<String>,
    /// id。
    pub id: Option<String>,
    /// name。
    pub name: Option<String>,
    /// display_name。
    pub display_name: Option<String>,
    /// title。
    pub title: Option<String>,
    /// description。
    pub description: Option<String>,
    /// 是否默认模型。
    pub is_default: Option<bool>,
    /// 默认 reasoning effort。
    pub default_reasoning_effort: Option<String>,
    /// 默认 reasoning level。
    pub default_reasoning_level: Option<String>,
    /// 支持的 reasoning effort 列表。
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<BackendReasoningEffort>,
    /// 支持的 reasoning level 列表。
    #[serde(default)]
    pub supported_reasoning_levels: Vec<BackendReasoningEffort>,
    /// 输入模态。
    pub input_modalities: Option<Vec<String>>,
    /// 输出模态。
    pub output_modalities: Option<Vec<String>>,
    /// 是否支持 personality。
    pub supports_personality: Option<bool>,
    /// 升级提示。
    pub upgrade: Option<String>,
    /// contextWindow 别名。
    #[serde(alias = "contextWindow")]
    pub context_window: Option<u64>,
    /// maxContextWindow 别名。
    #[serde(alias = "maxContextWindow")]
    pub max_context_window: Option<u64>,
    /// maxOutputTokens 别名。
    #[serde(alias = "maxOutputTokens")]
    pub max_output_tokens: Option<u64>,
    /// truncationPolicy 别名。
    #[serde(alias = "truncationPolicy")]
    pub truncation_policy: Option<BackendTruncationPolicy>,
}

// ---------------------------------------------------------------------------
// 模型快照存储端口
// ---------------------------------------------------------------------------

/// 模型快照存储错误。
#[derive(Debug, Error)]
pub enum ModelSnapshotStoreError {
    /// 底层存储失败。
    #[error("model snapshot store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 模型快照存储结果类型。
pub type ModelSnapshotStoreResult<T> = Result<T, ModelSnapshotStoreError>;

/// 模型快照存储端口。
#[async_trait]
pub trait ModelSnapshotStore: Send + Sync + 'static {
    /// 用单个计划快照替换同名快照。
    async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()>;

    /// 列出所有计划快照。
    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>>;
}

// ---------------------------------------------------------------------------
// 管理端模型刷新服务
// ---------------------------------------------------------------------------

/// 管理端模型服务。
#[derive(Clone)]
pub struct AdminModelService {
    models: Arc<ModelService>,
    accounts: Arc<dyn AccountStore>,
    installation_id: Option<String>,
}

impl AdminModelService {
    /// 构造服务。
    pub fn new(
        models: Arc<ModelService>,
        accounts: Arc<dyn AccountStore>,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            models,
            accounts,
            installation_id,
        }
    }

    /// 使用账号池中的账号刷新上游模型目录。
    pub async fn refresh_backend_models(
        &self,
        request_id: &str,
    ) -> Result<ModelRefreshResult, AdminModelError> {
        let accounts = self
            .accounts
            .list_pool_accounts()
            .await
            .map_err(|_| AdminModelError::ListAccounts)?;
        self.models
            .refresh_backend_models_with_installation_id(
                &accounts,
                request_id,
                self.installation_id.as_deref(),
            )
            .await
            .map_err(AdminModelError::from)
    }
}

/// 管理端模型刷新错误。
#[derive(Debug, Error)]
pub enum AdminModelError {
    #[error("failed to list accounts")]
    ListAccounts,
    #[error("no active accounts available for model refresh")]
    NoAccounts,
    #[error("model snapshot store is unavailable")]
    SnapshotStoreUnavailable,
    #[error("model upstream client is unavailable")]
    UpstreamClientUnavailable,
    #[error("failed to store model snapshot")]
    StoreSnapshot,
    #[error("failed to load model snapshots")]
    LoadSnapshots,
    #[error("all model refresh plans failed")]
    AllPlansFailed(ModelRefreshResult),
}

impl From<ModelServiceError> for AdminModelError {
    fn from(error: ModelServiceError) -> Self {
        match error {
            ModelServiceError::SnapshotStoreUnavailable => Self::SnapshotStoreUnavailable,
            ModelServiceError::UpstreamClientUnavailable => Self::UpstreamClientUnavailable,
            ModelServiceError::NoAccounts => Self::NoAccounts,
            ModelServiceError::StoreSnapshot => Self::StoreSnapshot,
            ModelServiceError::LoadSnapshots => Self::LoadSnapshots,
            ModelServiceError::AllPlansFailed(result) => Self::AllPlansFailed(result),
        }
    }
}

/// SQLite 模型快照存储。
#[derive(Clone)]
pub struct SqliteModelSnapshotStore {
    pool: sqlx::SqlitePool,
}

impl SqliteModelSnapshotStore {
    /// 使用给定连接池构造存储。
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ModelSnapshotStore for SqliteModelSnapshotStore {
    async fn replace_plan_snapshot(
        &self,
        snapshot: &ModelPlanSnapshot,
    ) -> ModelSnapshotStoreResult<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let models_json = serde_json::to_string(&snapshot.models).map_err(|e| {
            ModelSnapshotStoreError::OperationFailed {
                message: e.to_string(),
            }
        })?;
        sqlx::query(
            "insert or replace into model_plan_snapshots (plan_type, models_json, fetched_at) values (?, ?, ?)",
        )
        .bind(&snapshot.plan_type)
        .bind(&models_json)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| ModelSnapshotStoreError::OperationFailed {
            message: e.to_string(),
        })?;
        Ok(())
    }

    async fn list_plan_snapshots(&self) -> ModelSnapshotStoreResult<Vec<ModelPlanSnapshot>> {
        let rows = sqlx::query("select plan_type, models_json from model_plan_snapshots")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ModelSnapshotStoreError::OperationFailed {
                message: e.to_string(),
            })?;
        let mut snapshots = Vec::new();
        for row in rows {
            let plan_type: String = row.get("plan_type");
            let models_json: String = row.get("models_json");
            let models: Vec<CodexModelInfo> = serde_json::from_str(&models_json).map_err(|e| {
                ModelSnapshotStoreError::OperationFailed {
                    message: e.to_string(),
                }
            })?;
            snapshots.push(ModelPlanSnapshot { plan_type, models });
        }
        Ok(snapshots)
    }
}

// ---------------------------------------------------------------------------
// 模型目录聚合
// ---------------------------------------------------------------------------

use std::collections::BTreeSet;

const SERVICE_TIER_SUFFIXES: [&str; 2] = ["fast", "flex"];
const REASONING_EFFORT_SUFFIXES: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

/// 模型目录聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    models: Vec<CodexModelInfo>,
    model_aliases: BTreeMap<String, String>,
    model_plan_index: BTreeMap<String, Vec<String>>,
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
}

impl ModelCatalog {
    /// 从静态配置构造模型目录。
    pub fn from_config(config: &ModelConfig) -> Self {
        let model_aliases = normalize_aliases(&config.model_aliases);
        Self {
            models: Vec::new(),
            model_aliases,
            model_plan_index: BTreeMap::new(),
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
        let mut model_plan_index: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for snapshot in snapshots {
            for model in &snapshot.models {
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
            model_aliases,
            model_plan_index,
        }
    }

    /// 返回对外可见的模型列表副本。
    pub fn models(&self) -> Vec<CodexModelInfo> {
        self.models.clone()
    }

    /// 按模型 ID 查询模型信息。
    pub fn model_info(&self, model_id: &str) -> Option<CodexModelInfo> {
        self.models
            .iter()
            .find(|model| model.id == model_id)
            .cloned()
    }

    /// 返回 model -> plans allowlist。
    pub fn model_plan_allowlist(&self) -> BTreeMap<String, Vec<String>> {
        self.model_plan_index.clone()
    }

    /// 判断输入模型名是否在目录中可识别。
    pub fn is_recognized_model_name(&self, input: &str) -> bool {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return false;
        }
        if self.model_aliases.contains_key(trimmed) || self.model_info(trimmed).is_some() {
            return true;
        }

        let stripped = strip_known_model_suffixes(trimmed);
        if stripped.model_name == trimmed
            || (stripped.reasoning_effort.is_none() && stripped.service_tier.is_none())
        {
            return false;
        }
        self.model_aliases.contains_key(&stripped.model_name)
            || self.model_info(&stripped.model_name).is_some()
    }

    /// 解析外部传入的模型名，提取别名、推理强度和服务层级后缀。
    pub fn parse_model_name(&self, input: &str) -> ParsedModelName {
        let trimmed = input.trim();
        if self.model_aliases.contains_key(trimmed) || self.model_info(trimmed).is_some() {
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

    /// 将标准化模型名重新拼成展示名。
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

// ---------------------------------------------------------------------------
// 模型目录服务
// ---------------------------------------------------------------------------

use crate::upstream::transport::{CodexModelCatalogClient, CodexModelCatalogRequest};

/// 模型刷新摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRefreshResult {
    /// 成功刷新并写入的计划数。
    pub refreshed_plans: usize,
    /// 本次成功写入的模型数。
    pub model_count: usize,
    /// 刷新失败的计划数。
    pub failed_plans: usize,
}

/// 模型服务错误。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelServiceError {
    /// 没有注入快照存储。
    #[error("model snapshot store is unavailable")]
    SnapshotStoreUnavailable,
    /// 没有注入上游模型客户端。
    #[error("model catalog client is unavailable")]
    UpstreamClientUnavailable,
    /// 没有可用账号。
    #[error("no active accounts available for model refresh")]
    NoAccounts,
    /// 快照写入失败。
    #[error("failed to store model snapshot")]
    StoreSnapshot,
    /// 刷新后重新读取快照失败。
    #[error("failed to load model snapshots")]
    LoadSnapshots,
    /// 所有计划都刷新失败。
    #[error("all model refresh plans failed")]
    AllPlansFailed(ModelRefreshResult),
}

/// 模型到可用计划的映射。
pub type ModelPlanAllowlist = BTreeMap<String, Vec<String>>;

/// 可共享更新的模型计划映射缓存。
pub type SharedModelPlanAllowlist = Arc<tokio::sync::Mutex<ModelPlanAllowlist>>;

/// 模型目录服务。
#[derive(Clone)]
pub struct ModelService {
    config: ModelConfig,
    snapshot_store: Option<Arc<dyn ModelSnapshotStore>>,
    upstream_client: Option<Arc<dyn CodexModelCatalogClient>>,
    model_plan_allowlist: Option<SharedModelPlanAllowlist>,
}

impl ModelService {
    /// 构造模型服务。
    pub fn new(
        config: ModelConfig,
        snapshot_store: Option<Arc<dyn ModelSnapshotStore>>,
        upstream_client: Option<Arc<dyn CodexModelCatalogClient>>,
        model_plan_allowlist: Option<SharedModelPlanAllowlist>,
    ) -> Self {
        Self {
            config,
            snapshot_store,
            upstream_client,
            model_plan_allowlist,
        }
    }

    /// 返回模型目录的静态配置。
    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    /// 构造当前对外暴露的模型目录。
    pub async fn catalog(&self) -> ModelCatalog {
        let Some(snapshot_store) = self.snapshot_store.as_ref() else {
            return ModelCatalog::from_config(&self.config);
        };

        match snapshot_store.list_plan_snapshots().await {
            Ok(snapshots) if !snapshots.is_empty() => {
                ModelCatalog::from_config_and_snapshots(&self.config, &snapshots)
            }
            Ok(_) => ModelCatalog::from_config(&self.config),
            Err(error) => {
                tracing::warn!(error = %error, "加载模型快照失败，回退到静态目录");
                ModelCatalog::from_config(&self.config)
            }
        }
    }

    /// 刷新活跃账号对应的后端模型目录。
    pub async fn refresh_backend_models(
        &self,
        accounts: &[Account],
        request_id: &str,
    ) -> Result<ModelRefreshResult, ModelServiceError> {
        self.refresh_backend_models_with_installation_id(accounts, request_id, None)
            .await
    }

    /// 使用运行时 installation id 刷新活跃账号对应的后端模型目录。
    pub async fn refresh_backend_models_with_installation_id(
        &self,
        accounts: &[Account],
        request_id: &str,
        installation_id: Option<&str>,
    ) -> Result<ModelRefreshResult, ModelServiceError> {
        let snapshot_store = self
            .snapshot_store
            .as_ref()
            .ok_or(ModelServiceError::SnapshotStoreUnavailable)?;
        let upstream_client = self
            .upstream_client
            .as_ref()
            .ok_or(ModelServiceError::UpstreamClientUnavailable)?;

        let plan_accounts = distinct_active_plan_accounts(accounts);
        if plan_accounts.is_empty() {
            return Err(ModelServiceError::NoAccounts);
        }

        let mut result = ModelRefreshResult {
            refreshed_plans: 0,
            model_count: 0,
            failed_plans: 0,
        };

        for (plan_type, account) in plan_accounts {
            let request = CodexModelCatalogRequest {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                installation_id,
                plan_type: &plan_type,
            };

            let entries = match upstream_client.fetch_models(&request).await {
                Ok(entries) if !entries.is_empty() => entries,
                Ok(_) => {
                    result.failed_plans += 1;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(error = %error, plan_type, "刷新后端模型失败");
                    result.failed_plans += 1;
                    continue;
                }
            };

            let snapshot = ModelPlanSnapshot::from_backend_entries(plan_type, entries);
            result.model_count += snapshot.models.len();
            snapshot_store
                .replace_plan_snapshot(&snapshot)
                .await
                .map_err(map_store_snapshot_error)?;
            result.refreshed_plans += 1;
        }

        if result.refreshed_plans == 0 {
            return Err(ModelServiceError::AllPlansFailed(result));
        }

        let allowlist = self.model_plan_allowlist_from_store().await?;
        if let Some(shared_allowlist) = &self.model_plan_allowlist {
            *shared_allowlist.lock().await = allowlist;
        }

        Ok(result)
    }

    /// 读取当前缓存的 model -> plans allowlist。
    pub async fn model_plan_allowlist(&self) -> Result<ModelPlanAllowlist, ModelServiceError> {
        self.model_plan_allowlist_from_store().await
    }

    async fn model_plan_allowlist_from_store(
        &self,
    ) -> Result<ModelPlanAllowlist, ModelServiceError> {
        let snapshot_store = self
            .snapshot_store
            .as_ref()
            .ok_or(ModelServiceError::SnapshotStoreUnavailable)?;
        let snapshots = snapshot_store
            .list_plan_snapshots()
            .await
            .map_err(map_load_snapshots_error)?;
        Ok(
            ModelCatalog::from_config_and_snapshots(&self.config, &snapshots)
                .model_plan_allowlist(),
        )
    }
}

fn distinct_active_plan_accounts(accounts: &[Account]) -> Vec<(String, Account)> {
    let mut by_plan = BTreeMap::new();

    for account in accounts {
        if account.status != AccountStatus::Active {
            continue;
        }

        let plan_type = account
            .plan_type
            .as_deref()
            .unwrap_or("unknown")
            .to_string();
        by_plan.entry(plan_type).or_insert_with(|| account.clone());
    }

    by_plan.into_iter().collect()
}

fn map_store_snapshot_error(_: ModelSnapshotStoreError) -> ModelServiceError {
    ModelServiceError::StoreSnapshot
}

fn map_load_snapshots_error(_: ModelSnapshotStoreError) -> ModelServiceError {
    ModelServiceError::LoadSnapshots
}
