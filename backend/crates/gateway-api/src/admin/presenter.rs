//! Admin API 的展示层辅助：数值格式化。
//!
//! 按 docs/project-redundancy-boundary-audit.md BE-02 的目标，display 字段最终
//! 迁移到前端；过渡期统一由本模块提供，避免 accounts/observability 各自维护。

/// 千分位格式化。
#[must_use]
pub fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output.chars().rev().collect()
}

/// 紧凑格式化：≥1000 时用 K/M/B/T/P 后缀，否则千分位。
#[must_use]
pub fn format_compact_number(value: u64) -> String {
    if value < 1_000 {
        return format_number(value);
    }
    for (suffix, threshold) in [
        ("P", 1_000_000_000_000_000_u64),
        ("T", 1_000_000_000_000_u64),
        ("B", 1_000_000_000_u64),
        ("M", 1_000_000_u64),
        ("K", 1_000_u64),
    ] {
        if value >= threshold {
            let scaled = value as f64 / threshold as f64;
            return format!("{scaled:.1}{suffix}").replace(".0", "");
        }
    }
    format_number(value)
}
