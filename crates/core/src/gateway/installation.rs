//! Installation ID 的纯规则。
//!
//! 该模块只负责 UUID 格式校验和生成。`~/.codex/installation_id` 与数据目录
//! 持久化属于平台文件系统职责，放在 `platform::storage` 中。

use uuid::Uuid;

/// 生成新的 installation ID。
pub fn generate_installation_id() -> String {
    Uuid::new_v4().to_string()
}

/// 解析并规范化 installation ID。
///
/// 输入会先去除首尾空白；非 UUID 字符串返回 `None`。
pub fn parse_installation_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    Uuid::parse_str(trimmed).ok()?;
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn parse_installation_id_should_trim_and_accept_uuid() {
        let id = Uuid::new_v4().to_string();

        let parsed = parse_installation_id(&format!("  {id}\n"));

        assert_eq!(parsed, Some(id));
    }

    #[test]
    fn parse_installation_id_should_reject_non_uuid() {
        let parsed = parse_installation_id("not-a-uuid");

        assert_eq!(parsed, None);
    }

    #[test]
    fn generate_installation_id_should_return_uuid() {
        let generated = generate_installation_id();

        assert!(Uuid::parse_str(&generated).is_ok());
    }
}
