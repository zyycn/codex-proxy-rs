//! 发行物交换与失败补偿的模块门面

mod paths;
mod release;

pub(crate) use release::{
    backup_path_for, remove_dir_all, replace_release_files, rollback_official_plugins_dir,
    rollback_release,
};
