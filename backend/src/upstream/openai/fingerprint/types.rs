//! OpenAI 客户端指纹领域类型。

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    /// 客户端来源名。
    pub originator: String,
    /// 应用版本。
    pub app_version: String,
    /// 构建号。
    pub build_number: String,
    /// 平台名。
    pub platform: String,
    /// 架构名。
    pub arch: String,
    /// Chromium 主版本。
    pub chromium_version: String,
    /// User-Agent 模板。
    pub user_agent_template: String,
    /// 默认请求头。
    pub default_headers: IndexMap<String, String>,
    /// 请求头顺序。
    pub header_order: Vec<String>,
    /// DB 最后更新时间。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Fingerprint {
    /// 根据模板展开最终 User-Agent。
    pub fn user_agent(&self) -> String {
        self.user_agent_template
            .replace("{version}", &self.app_version)
            .replace("{platform}", &self.platform)
            .replace("{arch}", &self.arch)
    }

    /// 生成 `sec-ch-ua` 头值。
    pub fn sec_ch_ua(&self) -> String {
        format!(
            "\"Chromium\";v=\"{}\", \"Not:A-Brand\";v=\"24\"",
            self.chromium_version
        )
    }
}

/// 运行时共享指纹快照。

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    /// 最近检查时间。
    pub last_check: DateTime<Utc>,
    /// 最新版本。
    pub latest_version: Option<String>,
    /// 最新构建号。
    pub latest_build: Option<String>,
    /// 下载地址。
    pub download_url: Option<String>,
    /// 是否有可用更新。
    pub update_available: bool,
    /// 当前版本。
    pub current_version: String,
    /// 当前构建号。
    pub current_build: String,
}

/// Appcast 检查错误。
#[derive(Debug, Error)]
pub enum UpdateError {
    /// HTTP 请求失败。
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),
    /// 获取 appcast 失败。
    #[error("获取 appcast 失败，状态码: {0}")]
    AppcastFetch(u16),
    /// 解析 appcast 失败。
    #[error("解析 appcast 失败")]
    AppcastParse,
    /// JSON 序列化失败。
    #[error("JSON 序列化失败: {0}")]
    Json(#[from] serde_json::Error),
    /// 文件操作失败。
    #[error("文件操作失败: {0}")]
    Io(#[from] std::io::Error),
    /// 数据库存储失败。
    #[error("数据库操作失败: {0}")]
    Database(#[from] sqlx::Error),
}
