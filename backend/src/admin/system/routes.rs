//! 管理端系统路由入口。

pub(crate) use super::updater::{
    perform_update, restart, rollback, update_detail, update_event_stream, update_status, version,
};
