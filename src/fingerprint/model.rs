use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    pub originator: String,
    pub app_version: String,
    pub build_number: String,
    pub platform: String,
    pub arch: String,
    pub chromium_version: String,
    pub user_agent_template: String,
}

impl Fingerprint {
    pub fn default_for_tests() -> Self {
        Self {
            originator: "Codex Desktop".to_string(),
            app_version: "26.519.81530".to_string(),
            build_number: "3178".to_string(),
            platform: "darwin".to_string(),
            arch: "arm64".to_string(),
            chromium_version: "146".to_string(),
            user_agent_template:
                "Codex/{app_version} ({platform}; {arch}) Chromium/{chromium_version}".to_string(),
        }
    }

    pub fn user_agent(&self) -> String {
        self.user_agent_template
            .replace("{app_version}", &self.app_version)
            .replace("{platform}", &self.platform)
            .replace("{arch}", &self.arch)
            .replace("{chromium_version}", &self.chromium_version)
    }
}
