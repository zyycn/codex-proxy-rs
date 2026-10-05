//! 宿主回调的数据合同；这里只传递数据，不持有宿主服务或执行资源

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::model::ExecutionEvent;

const MODEL_EVENT_BATCH_PREFIX: [u8; 4] = *b"HME1";
const MAX_MODEL_EVENT_BATCH_BYTES: usize = 8 * 1024 * 1024;

// 凭据文档与账号资料不可通过 Debug 写入诊断
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialFacts {
    pub name: String,
    pub authentication_kind: String,
    pub material: Map<String, Value>,
    pub email: Option<String>,
    pub upstream_user_id: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    #[serde(default)]
    pub has_refresh_token: bool,
    pub access_token_expires_at_ms: Option<i64>,
    pub next_refresh_at_ms: Option<i64>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// event 必须是代码中固定的事件标识，不能填入凭据、URL 或请求内容
/// 宿主只记录 event 的长度和摘要，不因标识字符合法而放行明文
/// fields 由宿主按诊断规则脱敏；不能通过自定义字段声明内容可信
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogRequest {
    pub event: String,
    #[serde(default)]
    pub level: LogLevel,
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, serde_json::Value>,
}

/// 超出日志预算时直接丢弃，不等待、不排队，插件不应重试
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogResult {
    /// 是否交给有界异步日志写入；不保证立即落盘，最终级别过滤仍由宿主决定
    pub recorded: bool,
}

/// URL、头和正文可能携带敏感材料，不提供内容型 Debug
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub stream: Option<String>,
}

/// 宿主账号列表查询；cursor 是上次结果返回的稳定账号 ID
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthListRequest {
    pub provider_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: u16,
}

/// 不含原始凭据的账号运行投影
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthRuntimeAccount {
    pub account_id: String,
    pub provider_id: String,
    pub credential_revision: u64,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub authentication_kind: String,
    pub enabled: bool,
    pub credential_state: String,
    pub has_refresh_token: bool,
    pub access_token_expires_at_ms: Option<i64>,
    pub next_refresh_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthListResult {
    pub accounts: Vec<AuthRuntimeAccount>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthGetRequest {
    pub account_id: String,
}

/// 原始凭据只在 RPC 二进制载荷中传输，故意不实现 `Debug`
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthCredential {
    pub account_id: String,
    pub provider_id: String,
    pub credential_revision: u64,
    pub facts: CredentialFacts,
}

/// 新建账号由宿主生成 ID；替换账号必须提交精确 credential revision
/// 凭据只在 RPC 二进制载荷中传输，故意不实现 `Debug`
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthSaveRequest {
    Create {
        provider_id: String,
        facts: CredentialFacts,
    },
    Replace {
        account_id: String,
        credential_revision: u64,
        facts: CredentialFacts,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSaveResult {
    pub account_id: String,
    pub credential_revision: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamRead {
    pub stream: String,
    pub maximum_bytes: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamClose {
    pub stream: String,
}

/// 模型执行回调可交给 Core 的稳定操作；Provider 私有端点不属于模型回调
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelOperation {
    Generate,
    GenerateImage,
    EditImage,
    Search,
}

/// 模型执行回调的请求元数据；正文始终使用 RPC 的独立二进制载荷
///
/// Provider/账号用于指定执行目标；显式 Key 决定本次模型执行身份
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelExecuteRequest {
    /// 管理、命令、维护、认证与观察调用显式选择 Key；数据请求内的子调用继承父请求身份
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_id: Option<String>,
    pub model: String,
    pub protocol: String,
    pub operation: ModelOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
}

/// 按本次选择的 Key 查询可见模型，不读取或返回 Key 明文
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelListRequest {
    pub client_key_id: String,
    pub protocol: String,
    pub client_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelListResult {
    pub models: Vec<String>,
}

/// 分页读取 Key 的非秘密信息，用于管理页和命令行选择执行身份
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyListRequest {
    pub cursor: Option<String>,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientKey {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyListResult {
    pub keys: Vec<ClientKey>,
    pub next_cursor: Option<String>,
}

/// 非流式子请求已经完成并由 Core 提交；事件位于二进制载荷中的一个 HME1 批次
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelExecuteResult {
    pub request_id: String,
    pub events: u32,
}

/// 流式子请求句柄只能在创建它的父调用上下文中使用
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelStreamResult {
    pub request_id: String,
    pub stream: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelStreamReadRequest {
    pub stream: String,
    pub maximum_bytes: u32,
}

/// `end=true` 表示 Core 已完成终态与结算，载荷可以为空
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelStreamReadResult {
    pub events: u32,
    pub end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelStreamCloseRequest {
    pub stream: String,
}

/// 查询 Provider 已经保存的不可逆亲和键；宿主不会从任意会话文本推导假命中
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffinityLookupRequest {
    pub provider: String,
    pub key: String,
}

/// 命中只返回账号偏好；后续模型调用仍由 Core 复核账号资格并取得租约
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffinityLookupResult {
    pub account_id: Option<String>,
}

/// 模型执行回调的有界事件批次
/// 单个事件使用 GPE2 封套，批次只增加
/// 长度索引，不把原生正文改写成 JSON/base64
#[derive(Clone, Default)]
pub struct ModelEventBatch {
    pub events: Vec<ExecutionEvent>,
}

impl ModelEventBatch {
    /// # Errors
    ///
    /// 事件编码无效、数量/长度溢出或总批次超过 8 MiB 时返回错误
    pub fn encode(self) -> Result<Vec<u8>, ModelEventBatchError> {
        let count = u32::try_from(self.events.len()).map_err(|_| ModelEventBatchError)?;
        let mut encoded = Vec::with_capacity(self.events.len());
        let mut total = 8_usize
            .checked_add(
                self.events
                    .len()
                    .checked_mul(4)
                    .ok_or(ModelEventBatchError)?,
            )
            .ok_or(ModelEventBatchError)?;
        for event in self.events {
            let event = event.encode().map_err(|_| ModelEventBatchError)?;
            let _ = u32::try_from(event.len()).map_err(|_| ModelEventBatchError)?;
            total = total.checked_add(event.len()).ok_or(ModelEventBatchError)?;
            if total > MAX_MODEL_EVENT_BATCH_BYTES {
                return Err(ModelEventBatchError);
            }
            encoded.push(event);
        }
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(&MODEL_EVENT_BATCH_PREFIX);
        bytes.extend_from_slice(&count.to_be_bytes());
        for event in &encoded {
            bytes.extend_from_slice(
                &u32::try_from(event.len())
                    .map_err(|_| ModelEventBatchError)?
                    .to_be_bytes(),
            );
        }
        for event in encoded {
            bytes.extend_from_slice(&event);
        }
        Ok(bytes)
    }

    /// # Errors
    ///
    /// 版本、长度索引、事件封套或总批次无效时返回错误
    pub fn decode(bytes: &[u8]) -> Result<Self, ModelEventBatchError> {
        if bytes.len() < 8
            || bytes.len() > MAX_MODEL_EVENT_BATCH_BYTES
            || bytes[..4] != MODEL_EVENT_BATCH_PREFIX
        {
            return Err(ModelEventBatchError);
        }
        let count = usize::try_from(u32::from_be_bytes(
            bytes[4..8].try_into().map_err(|_| ModelEventBatchError)?,
        ))
        .map_err(|_| ModelEventBatchError)?;
        let payload_start = 8_usize
            .checked_add(count.checked_mul(4).ok_or(ModelEventBatchError)?)
            .ok_or(ModelEventBatchError)?;
        if payload_start > bytes.len() {
            return Err(ModelEventBatchError);
        }
        let mut cursor = payload_start;
        let mut events = Vec::with_capacity(count);
        for chunk in bytes[8..payload_start].chunks_exact(4) {
            let length = usize::try_from(u32::from_be_bytes(
                chunk.try_into().map_err(|_| ModelEventBatchError)?,
            ))
            .map_err(|_| ModelEventBatchError)?;
            let end = cursor.checked_add(length).ok_or(ModelEventBatchError)?;
            let event = bytes.get(cursor..end).ok_or(ModelEventBatchError)?;
            events.push(ExecutionEvent::decode(event).map_err(|_| ModelEventBatchError)?);
            cursor = end;
        }
        if cursor != bytes.len() || events.len() != count {
            return Err(ModelEventBatchError);
        }
        Ok(Self { events })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("host model event batch is invalid or exceeds its limit")]
pub struct ModelEventBatchError;

/// 私有状态读取；值只通过宿主绑定的实例与命名空间解析
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateGetRequest {
    pub namespace: String,
    pub key: String,
}

/// 状态值可能包含敏感插件数据，故意不实现 `Debug`
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateRecord {
    pub value: serde_json::Value,
    pub version: u64,
    pub schema_version: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateGetResult {
    pub record: Option<StateRecord>,
}

/// `expected_version = None` 表示仅当键不存在时创建；更新必须提交精确版本
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatePutRequest {
    pub namespace: String,
    pub key: String,
    pub value: serde_json::Value,
    pub expected_version: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatePutResult {
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDeleteRequest {
    pub namespace: String,
    pub key: String,
    pub expected_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDeleteResult {
    pub deleted: bool,
}

/// 宿主控制的迁移批次；插件不能选择源记录、目标实例或提交边界
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMigrationRequest {
    pub namespace: String,
    pub from_schema_version: u32,
    pub to_schema_version: u32,
    pub records: Vec<StateMigrationRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMigrationRecord {
    pub key: String,
    pub value: serde_json::Value,
    pub version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMigrationResult {
    pub changes: Vec<StateMigrationChange>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum StateMigrationChange {
    Keep {
        key: String,
    },
    Replace {
        key: String,
        value: serde_json::Value,
    },
    Delete {
        key: String,
    },
}

impl StateMigrationChange {
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Keep { key } | Self::Replace { key, .. } | Self::Delete { key } => key,
        }
    }
}
