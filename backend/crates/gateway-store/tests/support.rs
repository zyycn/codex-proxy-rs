/// 读取集成测试基础设施地址（PostgreSQL / Redis）。
///
/// 本地未配置时返回 `None`，测试跳过；CI 中缺失则直接失败——
/// 静默跳过产生的绿灯与真实回归不可区分。
pub(crate) fn test_env(name: &str) -> Option<String> {
    let value = std::env::var(name).ok();
    assert!(
        value.is_some() || !running_in_ci(),
        "{name} must be set in CI: store integration tests are not allowed to skip silently"
    );
    value
}

/// 按 is-ci 约定判定 CI：`CI` 存在且非 falsey（`0`/`false`/空）。
/// 开发者用 `CI=false` 显式声明「非 CI」时不应把跳过变成 panic。
fn running_in_ci() -> bool {
    std::env::var("CI")
        .ok()
        .is_some_and(|value| !matches!(value.as_str(), "" | "0" | "false"))
}
