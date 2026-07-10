//! Redis 运行态连接与键前缀。

use redis::{aio::ConnectionManager, AsyncCommands};

/// Redis 运行态连接。
#[derive(Clone)]
pub struct RedisConnection {
    manager: ConnectionManager,
    key_prefix: String,
}

impl RedisConnection {
    /// 建立 Redis 连接。
    pub async fn connect(url: &str, key_prefix: impl Into<String>) -> redis::RedisResult<Self> {
        let client = redis::Client::open(url)?;
        let manager = client.get_connection_manager().await?;
        Ok(Self {
            manager,
            key_prefix: normalize_key_prefix(key_prefix.into()),
        })
    }

    /// 返回可独立使用的多路复用连接管理器。
    pub fn manager(&self) -> ConnectionManager {
        self.manager.clone()
    }

    /// 为业务键添加应用前缀。
    pub fn key(&self, suffix: &str) -> String {
        format!("{}{}", self.key_prefix, suffix)
    }

    /// 检查 Redis 连接是否可用。
    pub async fn ping(&self) -> redis::RedisResult<()> {
        let mut connection = self.manager.clone();
        let response: String = connection.ping().await?;
        if response == "PONG" {
            Ok(())
        } else {
            Err(redis::RedisError::from((
                redis::ErrorKind::UnexpectedReturnType,
                "unexpected Redis PING response",
                response,
            )))
        }
    }
}

fn normalize_key_prefix(mut prefix: String) -> String {
    if !prefix.ends_with(':') {
        prefix.push(':');
    }
    prefix
}
