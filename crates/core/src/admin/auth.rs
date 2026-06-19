//! 管理员认证领域服务。

/// 管理员登录请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLoginRequest {
    /// 用户名。
    pub username: String,
    /// 明文密码。
    pub password: String,
}

/// 管理员认证服务。
#[derive(Debug, Clone)]
pub struct AdminAuthService {
    default_username: String,
}

impl AdminAuthService {
    /// 构造管理员认证服务。
    pub fn new(default_username: impl Into<String>) -> Self {
        Self {
            default_username: default_username.into(),
        }
    }

    /// 判断用户名是否匹配配置中的管理员用户名。
    pub fn username_matches(&self, username: &str) -> bool {
        self.default_username == username
    }
}
