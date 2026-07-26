/// 读取集成测试基础设施地址（PostgreSQL / Redis）。
///
/// 本地未配置时返回 `None`，测试跳过；CI 中缺失则直接失败——
/// 静默跳过产生的绿灯与真实回归不可区分。
pub(crate) fn test_env(name: &str) -> Option<String> {
    let value = std::env::var(name).ok();
    assert!(
        value.is_some() || std::env::var_os("CI").is_none(),
        "{name} must be set in CI: store integration tests are not allowed to skip silently"
    );
    value
}
