//! Admin API 的展示层辅助：数值格式化。
//!
//! 按 docs/project-redundancy-boundary-audit.md BE-02 的目标，display 字段最终
//! 迁移到前端；过渡期统一由本模块提供，避免 accounts/observability 各自维护。

use gateway_core::accounting::Decimal;

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

/// 货币展示格式化。USD 常规金额保留两位，小于 1 美元时最多保留四位。
#[must_use]
pub fn format_decimal_currency(amount: &str, currency: &str) -> String {
    if currency != "USD" {
        return format!("{currency} {amount}");
    }

    const DECIMAL_SCALE: u128 = 10_000_000_000;
    let scaled = amount.parse::<Decimal>().unwrap_or_default().scaled();
    let precision = if scaled != 0 && scaled < DECIMAL_SCALE {
        4_u32
    } else {
        2_u32
    };
    let rounding_unit = 10_u128.pow(10 - precision);
    let rounded = (scaled + rounding_unit / 2) / rounding_unit;
    let display_scale = 10_u128.pow(precision);
    let whole = rounded / display_scale;
    let fraction = rounded % display_scale;
    let mut display = format!("{whole}.{fraction:0width$}", width = precision as usize);
    while display.ends_with('0')
        && display
            .split_once('.')
            .is_some_and(|(_, fraction)| fraction.len() > 2)
    {
        display.pop();
    }
    format!("${display}")
}
