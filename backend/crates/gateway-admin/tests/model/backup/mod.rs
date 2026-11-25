//! 备份领域模型（model/backup）的纯逻辑测试

mod contract;
use chrono::{TimeZone as _, Utc};
use secrecy::SecretString;

use gateway_admin::model::backup::{
    BackupSettings, BackupStatus, BackupStatusTransition, BackupStorageConfig, BackupTriggerKind,
    build_object_key,
};
