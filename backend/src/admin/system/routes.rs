//! 管理端系统路由入口。

pub(crate) use super::updater::{
    check_updates, perform_update, restart, rollback, update_event_stream, update_status, version,
};
