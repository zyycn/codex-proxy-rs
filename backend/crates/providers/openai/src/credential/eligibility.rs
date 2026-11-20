//! OpenAI 请求的账号认证、套餐与模型权限资格

use gateway_core::account::scope::FrozenAccountScope;
use gateway_core::account::{ProviderAccount, ProviderAccountId};

use super::types::CODEX_AUTHENTICATION_KIND_OAUTH;

/// 选号使用的模型与端点类别，原生 Images 和 Search 允许模型未知
#[derive(Clone, Copy)]
pub(crate) enum CodexSelectionModel<'a> {
    Search(Option<&'a str>),
    Requested(&'a str),
    Image(Option<&'a str>),
    Live(&'a str),
}

impl<'a> CodexSelectionModel<'a> {
    pub(crate) const fn model(self) -> Option<&'a str> {
        match self {
            Self::Requested(model) | Self::Live(model) => Some(model),
            Self::Image(model) | Self::Search(model) => model,
        }
    }

    pub(crate) fn allows_account(self, account: &ProviderAccount) -> bool {
        if account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH {
            // Live 绑定 ChatGPT OAuth 身份；API Key 不套用 ChatGPT 套餐资格
            return !matches!(self, Self::Live(_));
        }
        // API Key 兼容上游可以提供 Grok；ChatGPT OAuth 不能因共用 Provider 被选中
        // 使用路由映射后的模型，不限制未知别名或尚未被目录收录的 OpenAI 模型
        if self.model().is_some_and(|model| {
            let model = model.rsplit('/').next().unwrap_or(model);
            model.eq_ignore_ascii_case("grok")
                || model
                    .get(..5)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("grok-"))
        }) {
            return false;
        }
        // Images 与 Live 只排除明确的 Free，不把未知套餐推断为 Free
        // Images 对齐官方 Codex 的 image_generation_available
        // https://github.com/openai/codex/commit/0a0a9b6c8f
        !(matches!(self, Self::Image(_) | Self::Live(_))
            && account
                .plan_type()
                .is_some_and(|plan| plan.trim().eq_ignore_ascii_case("free")))
    }

    pub(crate) fn allows(self, scope: &FrozenAccountScope, account: &ProviderAccountId) -> bool {
        match self {
            Self::Requested(model)
            | Self::Live(model)
            | Self::Image(Some(model))
            | Self::Search(Some(model)) => scope.allows_model(account, model),
            // 缺失、重复或不可解析的模型不能绕过黑白名单；不猜上游默认值
            Self::Image(None) | Self::Search(None) => scope.allows_unidentified_model(account),
        }
    }
}
