use std::path::Path;

use super::{types::AppConfig, ConfigResult};

impl AppConfig {
    pub fn load() -> ConfigResult<Self> {
        Self::load_from_dir(".")
    }

    pub fn load_from_dir(config_dir: impl AsRef<Path>) -> ConfigResult<Self> {
        let config_dir = config_dir.as_ref();
        ::config::Config::builder()
            .add_source(::config::File::from(config_dir.join("config.yaml")).required(true))
            .add_source(::config::File::from(config_dir.join("local.yaml")).required(false))
            .add_source(::config::File::from(config_dir.join("local.yml")).required(false))
            .build()?
            .try_deserialize()
    }
}
