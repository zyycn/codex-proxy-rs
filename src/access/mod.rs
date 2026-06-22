//! 访问控制模块 —— 管理员会话与客户端 API Key 管理。
//!
//! 提供管理员登录认证、会话管理、客户端 API Key 服务以及 SQLite 存储适配器。

pub mod admin_session;
pub mod client_keys;

pub use admin_session::{
    session_expiry, AdminAuthService, AdminLoginSession, AdminSession, AdminSessionError,
    AdminSessionService, SqliteAdminSessionStore, StoredAdminUser,
};

pub use client_keys::{
    ClientKeyService, ClientKeyStore, ClientKeyStoreError, ClientKeyStoreResult,
    CreatedClientApiKey, SqliteClientKeyStore, StoredClientApiKey,
};
