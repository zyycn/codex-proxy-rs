//! Admin API 的展示层辅助：数值格式化
//!
//! 当前 wire 仍保留 display 字段；统一由本模块提供，避免 accounts/observability
//! 各自维护格式化规则

use gateway_core::metering::Decimal;

/// 千分位格式化
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

/// 紧凑格式化：≥1000 时用 K/M/B/T/P 后缀，否则千分位
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

/// 货币展示格式化
/// USD 常规金额最多保留两位，小于 1 美元时最多保留四位，小数末尾不补零
#[must_use]
pub fn format_decimal_currency(amount: &str, currency: &str) -> String {
    if currency != "USD" {
        return format!("{currency} {}", trim_decimal_zeros(amount));
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
    let display = format!("{whole}.{fraction:0width$}", width = precision as usize);
    format!("${}", trim_decimal_zeros(&display))
}

/// 百分比最多保留一位小数，整数百分比不补零
#[must_use]
pub(super) fn format_percentage(value: f64) -> String {
    let display = format!("{value:.1}");
    format!("{}%", trim_decimal_zeros(&display))
}

fn trim_decimal_zeros(value: &str) -> &str {
    if value.contains('.') {
        value.trim_end_matches('0').trim_end_matches('.')
    } else {
        value
    }
}
