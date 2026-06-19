use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// 上游请求指纹。
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
}

impl Fingerprint {
    /// 返回默认的 Codex Desktop 指纹。
    pub fn default_codex_desktop() -> Self {
        Self {
            originator: "Codex Desktop".to_string(),
            app_version: "26.519.81530".to_string(),
            build_number: "3178".to_string(),
            platform: "darwin".to_string(),
            arch: "arm64".to_string(),
            chromium_version: "146".to_string(),
            user_agent_template: "Codex Desktop/{version} ({platform}; {arch})".to_string(),
            default_headers: Self::default_headers(),
            header_order: Self::default_header_order(),
        }
    }

    /// 返回测试用默认指纹。
    pub fn default_for_tests() -> Self {
        Self::default_codex_desktop()
    }

    /// 返回默认请求头集合。
    pub fn default_headers() -> IndexMap<String, String> {
        let mut headers = IndexMap::new();
        headers.insert(
            "Accept-Encoding".to_string(),
            "gzip, deflate, br, zstd".to_string(),
        );
        headers.insert("Accept-Language".to_string(), "en-US,en;q=0.9".to_string());
        headers.insert("sec-ch-ua-mobile".to_string(), "?0".to_string());
        headers.insert("sec-ch-ua-platform".to_string(), "\"macOS\"".to_string());
        headers.insert("sec-fetch-site".to_string(), "same-origin".to_string());
        headers.insert("sec-fetch-mode".to_string(), "cors".to_string());
        headers.insert("sec-fetch-dest".to_string(), "empty".to_string());
        headers
    }

    /// 返回默认请求头顺序。
    pub fn default_header_order() -> Vec<String> {
        vec![
            "authorization".to_string(),
            "chatgpt-account-id".to_string(),
            "originator".to_string(),
            "x-openai-internal-codex-residency".to_string(),
            "x-client-request-id".to_string(),
            "x-codex-installation-id".to_string(),
            "x-codex-turn-state".to_string(),
            "openai-beta".to_string(),
            "user-agent".to_string(),
            "sec-ch-ua".to_string(),
            "sec-ch-ua-mobile".to_string(),
            "sec-ch-ua-platform".to_string(),
            "accept-encoding".to_string(),
            "accept-language".to_string(),
            "sec-fetch-site".to_string(),
            "sec-fetch-mode".to_string(),
            "sec-fetch-dest".to_string(),
            "content-type".to_string(),
            "accept".to_string(),
            "cookie".to_string(),
        ]
    }

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
