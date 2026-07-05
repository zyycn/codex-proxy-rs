//! 展示格式化辅助。

/// 将负数归零后转换为无符号展示数值。
pub fn nonnegative_i64_to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// 将可空负数归零后转换为无符号展示数值。
pub fn optional_nonnegative_i64_to_u64(value: Option<i64>) -> u64 {
    value.map(nonnegative_i64_to_u64).unwrap_or_default()
}

/// 使用中文界面常用千分位格式展示整数。
pub fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (index, ch) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            output.push(',');
        }
        output.push(ch);
    }
    output.chars().rev().collect()
}

/// 不带分隔符展示整数。
pub fn format_plain_number(value: u64) -> String {
    value.to_string()
}

/// 使用 K/M/B/T/P 后缀展示紧凑整数。
pub fn format_compact_number(value: u64) -> String {
    if value < 1_000 {
        return format_number(value);
    }

    for (unit, threshold) in [
        ("P", 1_000_000_000_000_000_u64),
        ("T", 1_000_000_000_000_u64),
        ("B", 1_000_000_000_u64),
        ("M", 1_000_000_u64),
        ("K", 1_000_u64),
    ] {
        if value >= threshold {
            return format_compact_scaled(value as f64 / threshold as f64, unit);
        }
    }

    format_number(value)
}

/// 使用紧凑格式展示 token 数。
pub fn format_tokens(value: u64) -> String {
    format_compact_number(value)
}

/// 以一位小数展示百分比数值。
pub fn format_percent(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.1}%")
    } else {
        "—".to_string()
    }
}

/// 展示百分比，并去掉无意义的尾随 0。
pub fn format_compact_percent(value: f64) -> String {
    if value.is_finite() {
        format!("{}%", trim_trailing_zeroes(&format!("{value:.1}")))
    } else {
        "—".to_string()
    }
}

/// 将 0 到 1 的比例展示为百分比。
pub fn format_rate(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format_percent(value * 100.0))
}

/// 使用紧凑货币格式展示美元成本。
pub fn format_cost(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    let abs = value.abs();
    if abs >= 1_000_000_000.0 {
        return format!("${:.2}B", value / 1_000_000_000.0);
    }
    if abs >= 1_000_000.0 {
        return format!("${:.2}M", value / 1_000_000.0);
    }
    if abs >= 1_000.0 {
        return format!("${:.2}K", value / 1_000.0);
    }
    format!("${value:.2}")
}

/// 展示每百万 token 的美元单价。
pub fn format_token_price(value: f64) -> String {
    if value.is_finite() {
        format!("${value:.4} / 1M Token")
    } else {
        "—".to_string()
    }
}

/// 展示倍率。
pub fn format_multiplier(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.2}x")
    } else {
        "—".to_string()
    }
}

/// 将毫秒耗时展示为 ms、s、min、h 或 d。
pub fn format_duration_ms(value: Option<i64>) -> String {
    let Some(value) = value.filter(|value| *value >= 0) else {
        return "—".to_string();
    };

    if value < 1_000 {
        return format!("{value} ms");
    }

    if value < 60_000 {
        let seconds = value as f64 / 1_000.0;
        return if seconds >= 10.0 {
            format!("{seconds:.1} s")
        } else {
            format!("{seconds:.2} s")
        };
    }

    if value < 3_600_000 {
        return format!("{:.1} min", value as f64 / 60_000.0);
    }

    if value < 86_400_000 {
        return format!("{:.1} h", value as f64 / 3_600_000.0);
    }

    format!("{:.1} d", value as f64 / 86_400_000.0)
}

/// 将浮点毫秒耗时四舍五入后展示。
pub fn format_duration_ms_f64(value: Option<f64>) -> String {
    let Some(value) = value.filter(|value| value.is_finite() && *value >= 0.0) else {
        return "—".to_string();
    };
    format_duration_ms(Some(value.round() as i64))
}

fn format_compact_scaled(value: f64, unit: &str) -> String {
    let rounded = if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    format!("{}{unit}", trim_trailing_zeroes(&rounded))
}

/// 去掉小数字符串中无意义的尾随 0 和小数点。
pub fn trim_trailing_zeroes(value: &str) -> String {
    value
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
