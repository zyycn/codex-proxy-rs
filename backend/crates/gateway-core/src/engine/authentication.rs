//! 数据面入口认证计划；插件只产生 principal，Core 解析既有 Client Key 策略

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, RwLock, Weak},
};

use futures::future::BoxFuture;

use crate::{
    policy::{ClientApiKeyId, PlaintextClientApiKey},
    runtime::extensions::{ExtensionSetId, ExtensionSetReference},
};

pub const MAXIMUM_AUTHORIZATION_BYTES: usize = 8 * 1024;

/// API 从数据面 Authorization 头构造的有界认证信封
#[derive(Clone, PartialEq, Eq)]
pub struct ClientAuthenticationRequest {
    settings: Option<crate::settings::RequestSettings>,
    authorization: Arc<str>,
    native_bearer: Option<PlaintextClientApiKey>,
}

impl ClientAuthenticationRequest {
    #[must_use]
    pub fn with_settings(mut self, settings: crate::settings::RequestSettings) -> Self {
        self.settings = Some(settings);
        self
    }

    #[must_use]
    pub fn settings(&self) -> Option<&crate::settings::RequestSettings> {
        self.settings.as_ref()
    }

    pub(crate) fn take_settings(&mut self) -> Option<crate::settings::RequestSettings> {
        self.settings.take()
    }

    /// 创建入口认证信封；HTTP 语法与缺失头仍由 API owner 负责
    pub fn new(authorization: impl Into<String>) -> Result<Self, ClientAuthenticationRequestError> {
        let authorization = authorization.into();
        if authorization.is_empty()
            || authorization.len() > MAXIMUM_AUTHORIZATION_BYTES
            || !authorization.is_ascii()
            || authorization.chars().any(char::is_control)
        {
            return Err(ClientAuthenticationRequestError);
        }
        let native_bearer = authorization
            .strip_prefix("Bearer ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| PlaintextClientApiKey::new(value.to_owned()).ok());
        Ok(Self {
            settings: None,
            authorization: authorization.into(),
            native_bearer,
        })
    }

    /// 原生调用方显式构造 Bearer 信封；仍经过同一长度与字符校验
    pub fn bearer(plaintext: &str) -> Result<Self, ClientAuthenticationRequestError> {
        Self::new(format!("Bearer {plaintext}"))
    }

    #[must_use]
    pub fn authorization(&self) -> &str {
        &self.authorization
    }

    #[must_use]
    pub(crate) const fn native_bearer(&self) -> Option<&PlaintextClientApiKey> {
        self.native_bearer.as_ref()
    }
}

impl fmt::Debug for ClientAuthenticationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientAuthenticationRequest")
            .field("authorization", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("client authorization is invalid")]
pub struct ClientAuthenticationRequestError;

/// 插件认证结果不携带宿主执行身份
#[derive(Clone, PartialEq, Eq)]
pub enum FrontendAuthenticationDecision {
    Authenticated { principal: String },
    NotMatched,
    Rejected,
}

impl fmt::Debug for FrontendAuthenticationDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authenticated { .. } => formatter
                .debug_struct("Authenticated")
                .field("principal", &"[REDACTED]")
                .finish(),
            Self::NotMatched => formatter.write_str("NotMatched"),
            Self::Rejected => formatter.write_str("Rejected"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("frontend authentication provider is unavailable")]
pub struct FrontendAuthenticationError;

/// 一个发布代次中唯一的入口认证计划
pub trait FrontendAuthenticationPlan: Send + Sync {
    fn authenticate<'a>(
        &'a self,
        request: &'a ClientAuthenticationRequest,
    ) -> BoxFuture<'a, Result<FrontendAuthenticationDecision, FrontendAuthenticationError>>;

    /// 映射由管理员配置冻结，插件结果不能选择 Key ID
    fn client_key_id(&self, principal: &str) -> Option<ClientApiKeyId>;

    /// 独占计划不允许 `NotMatched` 回退原生 Bearer
    fn exclusive(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("frontend authentication generation is already registered")]
pub struct FrontendAuthenticationRegistrationError;

/// 按已发布集合解析的非拥有索引；旧代次由快照和在途调用保活
#[derive(Clone, Default)]
pub struct FrontendAuthenticationExtensionIndex {
    sets: Arc<RwLock<BTreeMap<ExtensionSetId, Weak<dyn FrontendAuthenticationPlan>>>>,
}

impl FrontendAuthenticationExtensionIndex {
    pub fn register(
        &self,
        id: ExtensionSetId,
        plan: Arc<dyn FrontendAuthenticationPlan>,
    ) -> Result<Arc<dyn FrontendAuthenticationPlan>, FrontendAuthenticationRegistrationError> {
        let mut sets = self
            .sets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.retain(|_, plan| plan.strong_count() > 0);
        if sets.contains_key(&id) {
            return Err(FrontendAuthenticationRegistrationError);
        }
        sets.insert(id, Arc::downgrade(&plan));
        Ok(plan)
    }

    #[must_use]
    pub fn resolve(
        &self,
        generation: &ExtensionSetReference,
    ) -> Option<Arc<dyn FrontendAuthenticationPlan>> {
        self.sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(generation.id())
            .and_then(Weak::upgrade)
    }
}
