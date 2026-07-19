//! Codex 官方模型目录 wire 与安全快照。

use std::collections::BTreeSet;
use std::num::NonZeroU64;

use gateway_core::routing::UpstreamModelId;
use reqwest::header::{ETAG, HeaderMap};
use serde::Deserialize;

/// 单次 Codex 模型目录响应允许的最大字节数。
pub const MAX_CODEX_MODEL_CATALOG_BYTES: usize = 1024 * 1024;

const MAX_CATALOG_MODELS: usize = 2_048;
const MAX_DISPLAY_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_ETAG_BYTES: usize = 256;

/// 上游目录对一项能力给出的明确证据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexCatalogCapabilityEvidence {
    /// 上游明确声明原生支持。
    DeclaredNative,
    /// 上游明确声明不支持。
    DeclaredUnsupported,
    /// 上游没有提供可依赖的声明。
    Unknown,
}

impl CodexCatalogCapabilityEvidence {
    fn from_wire(value: Option<bool>) -> Self {
        match value {
            Some(true) => Self::DeclaredNative,
            Some(false) => Self::DeclaredUnsupported,
            None => Self::Unknown,
        }
    }
}

/// Codex 目录中允许进入控制面的能力证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexCatalogCapabilities {
    responses_api: CodexCatalogCapabilityEvidence,
    reasoning: CodexCatalogCapabilityEvidence,
    parallel_tool_calls: CodexCatalogCapabilityEvidence,
    text_input: CodexCatalogCapabilityEvidence,
    image_input: CodexCatalogCapabilityEvidence,
    image_detail_original: CodexCatalogCapabilityEvidence,
    web_search: CodexCatalogCapabilityEvidence,
    verbosity: CodexCatalogCapabilityEvidence,
    reasoning_efforts: Vec<String>,
}

impl CodexCatalogCapabilities {
    /// 返回 Responses API 支持证据。
    #[must_use]
    pub const fn responses_api(&self) -> CodexCatalogCapabilityEvidence {
        self.responses_api
    }

    /// 返回 reasoning 支持证据。
    #[must_use]
    pub const fn reasoning(&self) -> CodexCatalogCapabilityEvidence {
        self.reasoning
    }

    /// 返回并行工具调用支持证据。
    #[must_use]
    pub const fn parallel_tool_calls(&self) -> CodexCatalogCapabilityEvidence {
        self.parallel_tool_calls
    }

    /// 返回文本输入支持证据。
    #[must_use]
    pub const fn text_input(&self) -> CodexCatalogCapabilityEvidence {
        self.text_input
    }

    /// 返回图片输入支持证据。
    #[must_use]
    pub const fn image_input(&self) -> CodexCatalogCapabilityEvidence {
        self.image_input
    }

    /// 返回原图 detail 支持证据。
    #[must_use]
    pub const fn image_detail_original(&self) -> CodexCatalogCapabilityEvidence {
        self.image_detail_original
    }

    /// 返回 Web search 支持证据。
    #[must_use]
    pub const fn web_search(&self) -> CodexCatalogCapabilityEvidence {
        self.web_search
    }

    /// 返回 verbosity 支持证据。
    #[must_use]
    pub const fn verbosity(&self) -> CodexCatalogCapabilityEvidence {
        self.verbosity
    }

    /// 返回上游明确列出的 reasoning effort。
    #[must_use]
    pub fn reasoning_efforts(&self) -> &[String] {
        &self.reasoning_efforts
    }
}

/// Codex 目录中明确声明的模型限制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexCatalogLimits {
    context_window_tokens: Option<NonZeroU64>,
    max_context_window_tokens: Option<NonZeroU64>,
}

impl CodexCatalogLimits {
    /// 返回正常上下文窗口；缺失表示未知。
    #[must_use]
    pub const fn context_window_tokens(&self) -> Option<NonZeroU64> {
        self.context_window_tokens
    }

    /// 返回上游允许覆盖到的最大上下文窗口；缺失表示未知。
    #[must_use]
    pub const fn max_context_window_tokens(&self) -> Option<NonZeroU64> {
        self.max_context_window_tokens
    }
}

/// Codex 目录中允许持久化的原始元数据白名单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexCatalogMetadata {
    description: Option<String>,
    priority: Option<i32>,
    visibility: Option<CodexCatalogVisibility>,
}

impl CodexCatalogMetadata {
    /// 返回安全的模型说明。
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// 返回上游排序优先级。
    #[must_use]
    pub const fn priority(&self) -> Option<i32> {
        self.priority
    }

    /// 返回上游 picker 可见性。
    #[must_use]
    pub const fn visibility(&self) -> Option<CodexCatalogVisibility> {
        self.visibility
    }
}

/// Codex 官方 picker 可见性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexCatalogVisibility {
    /// 正常列出。
    List,
    /// 对普通 picker 隐藏。
    Hide,
    /// 不进入 picker。
    None,
}

/// 一个已完整校验的 Codex 真实模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexCatalogModel {
    request_model: UpstreamModelId,
    display_name: String,
    capabilities: CodexCatalogCapabilities,
    limits: CodexCatalogLimits,
    metadata: CodexCatalogMetadata,
}

impl CodexCatalogModel {
    /// 返回实际写入上游请求的模型 slug。
    #[must_use]
    pub const fn request_model(&self) -> &UpstreamModelId {
        &self.request_model
    }

    /// 返回上游展示名。
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回能力证据。
    #[must_use]
    pub const fn capabilities(&self) -> &CodexCatalogCapabilities {
        &self.capabilities
    }

    /// 返回明确限制。
    #[must_use]
    pub const fn limits(&self) -> &CodexCatalogLimits {
        &self.limits
    }

    /// 返回白名单元数据。
    #[must_use]
    pub const fn metadata(&self) -> &CodexCatalogMetadata {
        &self.metadata
    }
}

/// 一次完整成功的 Codex 远端模型快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModelCatalogSnapshot {
    models: Vec<CodexCatalogModel>,
    etag: Option<String>,
}

impl CodexModelCatalogSnapshot {
    /// 返回本轮全部模型。
    #[must_use]
    pub fn models(&self) -> &[CodexCatalogModel] {
        &self.models
    }

    /// 返回经过语法和长度白名单校验的 HTTP ETag。
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }
}

/// Codex 模型目录不满足完整快照约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodexModelCatalogError {
    /// 响应超过硬上限。
    #[error("Codex model catalog response exceeds the byte limit")]
    ResponseTooLarge,
    /// 顶层不是唯一受支持的 `{models:[...]}` wire。
    #[error("Codex model catalog response violates the official wire contract")]
    InvalidWire,
    /// 目录没有任何模型。
    #[error("Codex model catalog snapshot is empty")]
    EmptySnapshot,
    /// 目录模型数量超过安全上限。
    #[error("Codex model catalog contains too many models")]
    TooManyModels,
    /// 实际请求模型 slug 不合法。
    #[error("Codex model catalog contains an invalid request model slug")]
    InvalidModelSlug,
    /// 同一实际请求模型重复出现。
    #[error("Codex model catalog contains a duplicate request model slug")]
    DuplicateModelSlug,
    /// 展示或白名单元数据不合法。
    #[error("Codex model catalog contains invalid public metadata")]
    InvalidMetadata,
    /// 模型限制不是明确的正整数。
    #[error("Codex model catalog contains invalid model limits")]
    InvalidLimits,
    /// 能力声明不合法或自相矛盾。
    #[error("Codex model catalog contains invalid capability evidence")]
    InvalidCapabilities,
    /// ETag 不属于允许持久化的安全格式。
    #[error("Codex model catalog ETag is invalid")]
    InvalidEtag,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexModelsWire {
    models: Vec<CodexModelWire>,
}

#[derive(Debug, Deserialize)]
struct CodexModelWire {
    slug: String,
    display_name: String,
    description: Option<String>,
    priority: Option<i32>,
    visibility: Option<CodexCatalogVisibility>,
    supported_in_api: Option<bool>,
    supported_reasoning_levels: Option<Vec<CodexReasoningEffortWire>>,
    supports_parallel_tool_calls: Option<bool>,
    supports_image_detail_original: Option<bool>,
    supports_search_tool: Option<bool>,
    support_verbosity: Option<bool>,
    context_window: Option<i64>,
    max_context_window: Option<i64>,
    input_modalities: Option<Vec<CodexInputModalityWire>>,
}

#[derive(Debug, Deserialize)]
struct CodexReasoningEffortWire {
    effort: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CodexInputModalityWire {
    Text,
    Image,
}

/// 解析唯一受支持的 Codex 官方完整快照。
///
/// # Errors
///
/// 任一条目、分页信号、重复 slug、未知顶层字段或 ETag 不合法时整轮失败。
pub fn parse_codex_model_catalog(
    body: &[u8],
    etag: Option<&str>,
) -> Result<CodexModelCatalogSnapshot, CodexModelCatalogError> {
    if body.len() > MAX_CODEX_MODEL_CATALOG_BYTES {
        return Err(CodexModelCatalogError::ResponseTooLarge);
    }
    let wire: CodexModelsWire =
        serde_json::from_slice(body).map_err(|_| CodexModelCatalogError::InvalidWire)?;
    if wire.models.is_empty() {
        return Err(CodexModelCatalogError::EmptySnapshot);
    }
    if wire.models.len() > MAX_CATALOG_MODELS {
        return Err(CodexModelCatalogError::TooManyModels);
    }

    let etag = etag.map(validate_etag).transpose()?;
    let mut seen = BTreeSet::new();
    let mut models = Vec::with_capacity(wire.models.len());
    for model in wire.models {
        let model = normalize_model(model)?;
        if !seen.insert(model.request_model.as_str().to_owned()) {
            return Err(CodexModelCatalogError::DuplicateModelSlug);
        }
        models.push(model);
    }
    Ok(CodexModelCatalogSnapshot { models, etag })
}

pub(super) fn catalog_etag(headers: &HeaderMap) -> Result<Option<String>, CodexModelCatalogError> {
    headers
        .get(ETAG)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| CodexModelCatalogError::InvalidEtag)
                .and_then(validate_etag)
        })
        .transpose()
}

fn normalize_model(wire: CodexModelWire) -> Result<CodexCatalogModel, CodexModelCatalogError> {
    if !valid_model_slug(&wire.slug) {
        return Err(CodexModelCatalogError::InvalidModelSlug);
    }
    let request_model =
        UpstreamModelId::new(wire.slug).map_err(|_| CodexModelCatalogError::InvalidModelSlug)?;
    validate_public_text(&wire.display_name, MAX_DISPLAY_NAME_BYTES, false)?;
    if let Some(description) = wire.description.as_deref() {
        validate_public_text(description, MAX_DESCRIPTION_BYTES, true)?;
    }

    let context_window_tokens = optional_positive(wire.context_window)?;
    let max_context_window_tokens = optional_positive(wire.max_context_window)?;
    if context_window_tokens
        .zip(max_context_window_tokens)
        .is_some_and(|(context, maximum)| context > maximum)
    {
        return Err(CodexModelCatalogError::InvalidLimits);
    }

    let (reasoning, reasoning_efforts) = reasoning_evidence(wire.supported_reasoning_levels)?;
    let (text_input, image_input) = modality_evidence(wire.input_modalities)?;
    let image_detail_original =
        CodexCatalogCapabilityEvidence::from_wire(wire.supports_image_detail_original);
    if image_input == CodexCatalogCapabilityEvidence::DeclaredUnsupported
        && image_detail_original == CodexCatalogCapabilityEvidence::DeclaredNative
    {
        return Err(CodexModelCatalogError::InvalidCapabilities);
    }

    Ok(CodexCatalogModel {
        request_model,
        display_name: wire.display_name,
        capabilities: CodexCatalogCapabilities {
            responses_api: CodexCatalogCapabilityEvidence::from_wire(wire.supported_in_api),
            reasoning,
            parallel_tool_calls: CodexCatalogCapabilityEvidence::from_wire(
                wire.supports_parallel_tool_calls,
            ),
            text_input,
            image_input,
            image_detail_original,
            web_search: CodexCatalogCapabilityEvidence::from_wire(wire.supports_search_tool),
            verbosity: CodexCatalogCapabilityEvidence::from_wire(wire.support_verbosity),
            reasoning_efforts,
        },
        limits: CodexCatalogLimits {
            context_window_tokens,
            max_context_window_tokens,
        },
        metadata: CodexCatalogMetadata {
            description: wire.description,
            priority: wire.priority,
            visibility: wire.visibility,
        },
    })
}

fn optional_positive(value: Option<i64>) -> Result<Option<NonZeroU64>, CodexModelCatalogError> {
    value
        .map(|value| {
            u64::try_from(value)
                .ok()
                .and_then(NonZeroU64::new)
                .ok_or(CodexModelCatalogError::InvalidLimits)
        })
        .transpose()
}

fn reasoning_evidence(
    values: Option<Vec<CodexReasoningEffortWire>>,
) -> Result<(CodexCatalogCapabilityEvidence, Vec<String>), CodexModelCatalogError> {
    let Some(values) = values else {
        return Ok((CodexCatalogCapabilityEvidence::Unknown, Vec::new()));
    };
    let evidence = if values.is_empty() {
        CodexCatalogCapabilityEvidence::DeclaredUnsupported
    } else {
        CodexCatalogCapabilityEvidence::DeclaredNative
    };
    let mut seen = BTreeSet::new();
    let mut efforts = Vec::with_capacity(values.len());
    for value in values {
        if !valid_capability_atom(&value.effort) || !seen.insert(value.effort.clone()) {
            return Err(CodexModelCatalogError::InvalidCapabilities);
        }
        efforts.push(value.effort);
    }
    Ok((evidence, efforts))
}

fn modality_evidence(
    values: Option<Vec<CodexInputModalityWire>>,
) -> Result<
    (
        CodexCatalogCapabilityEvidence,
        CodexCatalogCapabilityEvidence,
    ),
    CodexModelCatalogError,
> {
    let Some(values) = values else {
        return Ok((
            CodexCatalogCapabilityEvidence::Unknown,
            CodexCatalogCapabilityEvidence::Unknown,
        ));
    };
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value) {
            return Err(CodexModelCatalogError::InvalidCapabilities);
        }
    }
    Ok((
        declared_presence(unique.contains(&CodexInputModalityWire::Text)),
        declared_presence(unique.contains(&CodexInputModalityWire::Image)),
    ))
}

fn declared_presence(value: bool) -> CodexCatalogCapabilityEvidence {
    if value {
        CodexCatalogCapabilityEvidence::DeclaredNative
    } else {
        CodexCatalogCapabilityEvidence::DeclaredUnsupported
    }
}

fn valid_model_slug(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_alphanumeric())
        && value.len() <= 256
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !value.starts_with("__")
}

fn valid_capability_atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_public_text(
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), CodexModelCatalogError> {
    if (!allow_empty && value.trim().is_empty())
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
    {
        return Err(CodexModelCatalogError::InvalidMetadata);
    }
    Ok(())
}

fn validate_etag(value: &str) -> Result<String, CodexModelCatalogError> {
    let tag = value.strip_prefix("W/").unwrap_or(value);
    let bytes = tag.as_bytes();
    if value.len() > MAX_ETAG_BYTES
        || bytes.len() < 2
        || bytes.first() != Some(&b'"')
        || bytes.last() != Some(&b'"')
        || !bytes[1..bytes.len() - 1]
            .iter()
            .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte))
    {
        return Err(CodexModelCatalogError::InvalidEtag);
    }
    Ok(value.to_owned())
}
