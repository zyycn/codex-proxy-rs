//! SQLite 存储初始化与路径辅助。

mod paths;
/// SQLite 连接初始化。
pub mod sqlite;

pub use paths::{
    codex_desktop_installation_id_path, data_dir, data_installation_id_path, ensure_data_dir,
    load_or_create_installation_id, load_or_create_installation_id_from_paths,
};
pub use sqlite::{connect_sqlite, SqlitePool};
