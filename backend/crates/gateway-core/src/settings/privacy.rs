//! 隐私策略配置、编译端口与无敏感值的执行结果，协议解析由 Provider 拥有

use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexPrivacyPolicy {
    pub enabled: bool,
    pub on_error: PrivacyFailureMode,
    pub rules: Vec<PrivacyRule>,
}

impl fmt::Debug for CodexPrivacyPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexPrivacyPolicy")
            .field("enabled", &self.enabled)
            .field("on_error", &self.on_error)
            .field("rule_count", &self.rules.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyFailureMode {
    #[default]
    SkipRule,
    RejectRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyScope {
    TurnMetadata,
    DesktopGitContext,
    EnvironmentText,
    RequestBody,
    RequestHeader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyAction {
    RegexReplace,
    SetValue,
    RenameKey,
    RemoveField,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivacyRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub scope: PrivacyScope,
    pub selector: String,
    pub action: PrivacyAction,
    /// 删除动作的 null 表示无条件删除，字符串模式只匹配目标字符串值
    pub pattern: Option<String>,
    pub replacement: String,
    pub value: Value,
    pub replace_all: bool,
    pub case_insensitive: bool,
    pub multi_line: bool,
}

impl fmt::Debug for PrivacyRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivacyRule")
            .field("scope", &self.scope)
            .field("action", &self.action)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// 编译和执行失败仅携带规则序号与静态原因，不回显表达式或请求内容
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(rename_all = "camelCase")]
#[error("privacy rule {rule_index}: {reason}")]
pub struct PrivacyError {
    pub rule_index: usize,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyRuleOutcome {
    pub rule_id: String,
    pub matches: usize,
    pub status: String,
    pub reason: Option<String>,
}

/// 已编译策略随快照冻结，调用方提供独立的本次出站副本
pub trait CompiledPrivacyPolicy: fmt::Debug + Send + Sync {
    fn apply(
        &self,
        body: &mut Value,
        headers: &mut http::HeaderMap,
        turn_metadata: &mut Option<String>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<PrivacyRuleOutcome>, PrivacyError>;
}

pub trait PrivacyPolicyCompiler: fmt::Debug + Send + Sync {
    fn compile(
        &self,
        policy: &CodexPrivacyPolicy,
    ) -> Result<Arc<dyn CompiledPrivacyPolicy>, PrivacyError>;
}

/// 预览只接受管理员主动提供的受限样本，不读取实际请求记录
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrivacyPreviewRequest {
    pub policy: CodexPrivacyPolicy,
    pub body: Value,
    pub headers: std::collections::BTreeMap<String, Vec<String>>,
    pub turn_metadata: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyPreviewResult {
    pub body: Value,
    pub headers: std::collections::BTreeMap<String, Vec<String>>,
    pub turn_metadata: Option<String>,
    pub outcomes: Vec<PrivacyRuleOutcome>,
}
