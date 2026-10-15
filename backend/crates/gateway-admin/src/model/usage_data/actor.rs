//! 由宿主传入的可信写入主体，不属于插件请求载荷

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageActor {
    Admin {
        subject: String,
    },
    Plugin {
        publisher: String,
        name: String,
        instance_id: String,
    },
    System,
}

impl super::UsageRecordChange {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Put { id, .. } | Self::Update { id, .. } | Self::Delete { id } => id,
        }
    }
}
