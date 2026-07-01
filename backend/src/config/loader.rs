use std::path::Path;

use super::types::AppConfig;

impl AppConfig {
    /// 从当前目录加载配置文件。
    pub fn load() -> Result<Self, ::config::ConfigError> {
        Self::load_from_dir(".")
    }

    /// 从指定目录加载配置文件。
    pub fn load_from_dir(config_dir: impl AsRef<Path>) -> Result<Self, ::config::ConfigError> {
        let config_dir = config_dir.as_ref();
        ::config::Config::builder()
            .add_source(::config::File::from(config_dir.join("config.yaml")).required(true))
            .build()?
            .try_deserialize()
    }
}
