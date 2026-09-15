//! 管理员配置的账号模型政策，与上游模型权限分开保存和判断。

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::validation::validate_text;

/// 一个账号最多配置的精确模型 ID 数量。
pub const MAX_ACCOUNT_ACCESS_MODELS: usize = 256;

/// 账号模型政策的匹配方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountModelAccessMode {
    #[default]
    All,
    Allowlist,
    Denylist,
}

/// 已校验的账号模型政策；私有字段保证所有边界使用相同的匹配规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountModelAccess {
    mode: AccountModelAccessMode,
    models: BTreeSet<String>,
}

/// 不携带原始输入，避免在错误边界回显配置正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "modelAccess requires all with no models, or allowlist/denylist with 1–256 exact model IDs (at most 256 bytes each)"
)]
pub struct InvalidAccountModelAccess;

impl Default for AccountModelAccess {
    fn default() -> Self {
        Self::all()
    }
}

impl AccountModelAccess {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            mode: AccountModelAccessMode::All,
            models: BTreeSet::new(),
        }
    }

    pub fn new(
        mode: AccountModelAccessMode,
        models: Vec<String>,
    ) -> Result<Self, InvalidAccountModelAccess> {
        if models.len() > MAX_ACCOUNT_ACCESS_MODELS
            || (mode == AccountModelAccessMode::All) != models.is_empty()
            || models.iter().any(|model| {
                model.trim() != model
                    || model.trim().is_empty()
                    || model.contains('*')
                    || validate_text(model, 256, true, None).is_err()
            })
        {
            return Err(InvalidAccountModelAccess);
        }
        Ok(Self {
            mode,
            models: models.into_iter().collect(),
        })
    }

    #[must_use]
    pub const fn mode(&self) -> AccountModelAccessMode {
        self.mode
    }

    #[must_use]
    pub fn models(&self) -> &BTreeSet<String> {
        &self.models
    }

    #[must_use]
    pub fn allows(&self, upstream_model: &str) -> bool {
        match self.mode {
            AccountModelAccessMode::All => true,
            AccountModelAccessMode::Allowlist => self.models.contains(upstream_model),
            AccountModelAccessMode::Denylist => !self.models.contains(upstream_model),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelAccessDocument {
    mode: AccountModelAccessMode,
    models: Vec<String>,
}

impl<'de> Deserialize<'de> for AccountModelAccess {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = ModelAccessDocument::deserialize(deserializer)?;
        Self::new(document.mode, document.models).map_err(serde::de::Error::custom)
    }
}
