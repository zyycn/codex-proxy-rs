use std::path::Path;

use super::types::AppConfig;
use crate::config::{ConfigWriteError, ConfigWriteResult};

impl AppConfig {
    /// 写入管理端设置到主配置文件。
    pub async fn write_settings_config(
        &self,
        config_path: impl AsRef<Path>,
    ) -> ConfigWriteResult<()> {
        let config_path = config_path.as_ref();
        if let Some(parent) = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ConfigWriteError::CreateDirectory)?;
        }

        let content = serde_yml::to_string(self).map_err(ConfigWriteError::Serialize)?;
        tokio::fs::write(config_path, content)
            .await
            .map_err(ConfigWriteError::Write)
    }
}
