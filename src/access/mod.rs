//! 访问控制模块 —— 管理员会话与客户端 API Key 管理。
//!
//! 提供管理员登录认证、会话管理、客户端 API Key 的端口定义、
//! 业务服务以及 SQLite 存储适配器。

pub mod admin_session;
pub mod client_keys;

// -- 从 admin_session 重新导出 --

pub use admin_session::{
    session_expiry, AdminAuthService, AdminLoginRequest, AdminSession, SqliteAdminSessionStore,
    StoredAdminUser,
};

// -- 从 client_keys 重新导出 --

pub use client_keys::{
    ClientKeyService, ClientKeyStore, ClientKeyStoreError, ClientKeyStoreResult,
    CreatedClientApiKey, SqliteClientKeyStore, StoredClientApiKey,
};
