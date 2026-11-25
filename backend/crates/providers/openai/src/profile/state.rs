//! OpenAI 请求身份快照与发行历史状态

use super::selection::{ClientKind, ClientPlatform, ClientRelease};
use super::{CodexBundledReleaseProfile, baseline, cli_release, platform_release, selection};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, RwLock};

/// Codex 上游请求身份快照
///
/// 预设按官方配套版本生成 UA；自定义身份保留完整 UA 及其配套请求头
/// 官方制品核验只更新独立的基线与预设资料，不改写已冻结的请求身份
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexWireProfile {
    pub client_kind: ClientKind,
    /// `originator` 请求头及 User-Agent 产品名
    pub originator: String,
    /// Core 版本，用于模型请求的 version 和 client_version
    pub codex_version: String,
    /// Desktop 应用版本，用于 app-server `clientInfo.version` 对应的 UA 后缀
    pub desktop_version: String,
    /// Desktop 制品构建号，仅用于发布对齐诊断
    pub desktop_build: String,
    /// Codex Core UA 中的目标操作系统类型
    pub os_type: String,
    /// Codex Core UA 中的目标操作系统版本
    pub os_version: String,
    /// Codex Core UA 中的目标架构
    pub arch: String,
    /// Codex Core UA 中的终端标记
    pub terminal: String,
    /// 入口专属或自定义的完整值；存在时不得用默认预设重新拼接
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_user_agent: Option<String>,
    /// Key 的安装身份种子，仅在本地按 Key 与账号派生出站身份
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_seed: Option<String>,
    /// 未配置时不发送 residency 头；不随制品版本更新而改变
    pub residency: Option<String>,
    /// 部署指定的账号路由头；未配置时不发送，不随客户端身份选择改变
    pub account_routing_override: Option<String>,
    /// 版本元组最后一次经制品核验的时间；不表示 TLS 传输已重新核验
    pub verified_at: DateTime<Utc>,
}

impl Default for CodexWireProfile {
    fn default() -> Self {
        let release = baseline::MACOS_RELEASE;
        let environment = baseline::MACOS_ENVIRONMENT;
        Self {
            client_kind: ClientKind::Desktop,
            originator: baseline::DESKTOP_ORIGINATOR.to_owned(),
            codex_version: release.core.version.to_owned(),
            desktop_version: release.version.to_owned(),
            desktop_build: release.build.to_owned(),
            os_type: environment.os_type.to_owned(),
            os_version: environment.os_version.to_owned(),
            arch: environment.arch.to_owned(),
            terminal: baseline::TERMINAL.to_owned(),
            exact_user_agent: None,
            installation_seed: None,
            residency: None,
            account_routing_override: None,
            verified_at: release.core.verified_at(),
        }
    }
}

impl CodexWireProfile {
    /// 原样返回已解析的完整 UA；默认预设按 bundled Core app-server 格式生成
    pub fn user_agent(&self) -> String {
        if let Some(user_agent) = &self.exact_user_agent {
            return user_agent.clone();
        }
        if self.client_kind == ClientKind::Cli {
            return format!(
                "{}/{} ({} {}; {}) {}",
                self.originator,
                self.codex_version,
                self.os_type,
                self.os_version,
                self.arch,
                self.terminal
            );
        }
        format!(
            "{}/{} ({} {}; {}) {} ({}; {})",
            self.originator,
            self.codex_version,
            self.os_type,
            self.os_version,
            self.arch,
            self.terminal,
            self.originator,
            self.desktop_version,
        )
    }

    /// 按 Codex Desktop 暴露的稳定桌面 surface 格式构造 User-Agent
    ///
    /// Electron 网络栈会为 renderer 请求附加运行时 User-Agent；非 Electron transport
    /// 使用同一官方包中 `getDesktopUserAgent` 的稳定格式
    /// 它与 bundled Core 请求使用的
    /// 复合 User-Agent 是两个独立 surface
    #[must_use]
    pub fn desktop_user_agent(&self) -> String {
        format!(
            "{}/{} ({}; {})",
            self.originator, self.desktop_version, self.os_type, self.arch
        )
    }
}

/// 跨 Codex 上游请求共享的运行时请求画像
#[derive(Debug, Clone)]
pub struct CodexWireProfileState {
    profile: Arc<RwLock<CodexWireProfile>>,
    releases: Arc<RwLock<ClientReleases>>,
}

type ClientReleases = BTreeMap<(ClientKind, ClientPlatform, String), ClientReleaseObservation>;

/// 每渠道保留的发布观察深度：当前版本加上全部滞后档位。
const RELEASE_HISTORY_CAPACITY: usize = selection::MAX_VERSION_LAG as usize + 1;

#[derive(Debug, Clone, Default)]
struct ClientReleaseObservation {
    release: Option<ClientRelease>,
    /// 本渠道倒序观察到的发布序列（含当前版本），仅进程内存；重启后从种子与缓存重新积累。
    history: VecDeque<ClientRelease>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

/// 观察到的发布进入历史：同版本刷新队首，新版本压入队首；超出深度丢弃最旧一项。
fn observe_release(history: &mut VecDeque<ClientRelease>, release: ClientRelease) {
    if history
        .front()
        .is_some_and(|front| front.same_identity(&release))
    {
        history[0] = release;
    } else {
        history.push_front(release);
    }
    while history.len() > RELEASE_HISTORY_CAPACITY {
        history.pop_back();
    }
}

impl CodexWireProfileState {
    /// 从启动画像创建运行时状态
    pub fn new(profile: CodexWireProfile) -> Self {
        let state = Self {
            profile: Arc::new(RwLock::new(profile)),
            releases: Arc::default(),
        };
        // macOS arm64 的当前发布来自初始画像（默认快照或恢复缓存）；补齐观察表项，
        // 让滞后档位在首次 appcast 检查前也有历史可解析。
        let initial = state.snapshot();
        state.seed_client_release(
            ClientKind::Desktop,
            ClientPlatform::Macos,
            "arm64",
            ClientRelease {
                codex_version: initial.codex_version,
                desktop_version: Some(initial.desktop_version),
                desktop_build: Some(initial.desktop_build),
                verified_at: Some(initial.verified_at),
            },
        );
        cli_release::seed_releases(&state);
        platform_release::seed_releases(&state);
        state
    }

    pub(crate) fn effective_installation_id(
        &self,
        client_key_id: &gateway_core::policy::ClientApiKeyId,
        account_id: &gateway_core::account::ProviderAccountId,
        account_installation_id: &str,
    ) -> String {
        self.profile
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .effective_installation_id(client_key_id, account_id, account_installation_id)
    }

    /// 返回当前画像的独立快照，避免持锁执行网络请求
    pub fn snapshot(&self) -> CodexWireProfile {
        self.profile
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 原子发布同一 Desktop ZIP 中核验出的完整版本元组
    pub fn update_bundled_release(&self, release: &CodexBundledReleaseProfile) {
        let mut profile = self
            .profile
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        profile.codex_version.clone_from(&release.codex_version);
        profile.desktop_version.clone_from(&release.desktop_version);
        profile.desktop_build.clone_from(&release.desktop_build);
        profile.verified_at = release.verified_at;
    }

    pub fn client_release(
        &self,
        client: ClientKind,
        platform: ClientPlatform,
        arch: &str,
    ) -> Option<ClientRelease> {
        if client == ClientKind::Desktop && platform == ClientPlatform::Macos && arch == "arm64" {
            let profile = self.snapshot();
            return Some(ClientRelease {
                codex_version: profile.codex_version,
                desktop_version: Some(profile.desktop_version),
                desktop_build: Some(profile.desktop_build),
                verified_at: Some(profile.verified_at),
            });
        }
        self.releases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(client, platform, arch.to_owned()))
            .and_then(|state| state.release.clone())
    }

    /// 按滞后档位解析发布：取观察历史的第 `lag` 项，历史不足时回退最旧一项。
    ///
    /// 滞后档位不引入新的失败模式：历史缺失时调用方回退到当前发布。
    pub fn lagged_client_release(
        &self,
        client: ClientKind,
        platform: ClientPlatform,
        arch: &str,
        lag: u32,
    ) -> Option<ClientRelease> {
        self.releases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(client, platform, arch.to_owned()))
            .and_then(|state| {
                state
                    .history
                    .get(lag as usize)
                    .or_else(|| state.history.back())
                    .cloned()
            })
    }

    pub fn client_release_status(
        &self,
        client: ClientKind,
        platform: ClientPlatform,
        arch: &str,
    ) -> (Option<DateTime<Utc>>, Option<String>) {
        self.releases
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(client, platform, arch.to_owned()))
            .map_or((None, None), |state| {
                (state.checked_at, state.error.clone())
            })
    }

    pub(super) fn seed_client_release(
        &self,
        client: ClientKind,
        platform: ClientPlatform,
        arch: &str,
        release: ClientRelease,
    ) {
        let mut states = self
            .releases
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = states
            .entry((client, platform, arch.to_owned()))
            .or_default();
        // 种子与缓存恢复同样构成观察：更晚恢复的版本会排在种子之前，形成启动历史。
        observe_release(&mut state.history, release.clone());
        state.release = Some(release);
    }

    pub(crate) fn record_client_release(
        &self,
        client: ClientKind,
        platform: ClientPlatform,
        arch: &str,
        result: Result<ClientRelease, String>,
    ) {
        let mut states = self
            .releases
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = states
            .entry((client, platform, arch.to_owned()))
            .or_default();
        state.checked_at = Some(Utc::now());
        match result {
            Ok(release) => {
                state.release = Some(release.clone());
                state.error = None;
                observe_release(&mut state.history, release);
            }
            // 失败观察只更新状态，不改动发布历史。
            Err(error) => state.error = Some(error),
        }
    }
}
