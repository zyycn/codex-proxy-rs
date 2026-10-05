//! CLI duration 使用精确十进制纳秒，不经浮点转换或平台宽度截断

pub(super) fn parse(value: &str) -> Option<i64> {
    let (negative, mut remaining) = match value.as_bytes().first()? {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    if remaining == "0" {
        return Some(0);
    }
    let mut total = 0u128;
    let mut parts = 0;
    while !remaining.is_empty() {
        let end =
            remaining.find(|character: char| !character.is_ascii_digit() && character != '.')?;
        let number = &remaining[..end];
        remaining = &remaining[end..];
        let (unit, multiplier) = [
            ("ns", 1u128),
            ("us", 1_000),
            ("µs", 1_000),
            ("ms", 1_000_000),
            ("s", 1_000_000_000),
            ("m", 60_000_000_000),
            ("h", 3_600_000_000_000),
        ]
        .into_iter()
        .find(|(unit, _)| remaining.starts_with(unit))?;
        remaining = &remaining[unit.len()..];
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if whole.is_empty() && fraction.is_empty() || fraction.len() > 9 {
            return None;
        }
        let whole = if whole.is_empty() {
            0
        } else {
            whole.parse::<u128>().ok()?
        };
        let fraction = if fraction.is_empty() {
            0
        } else {
            let numerator = fraction.parse::<u128>().ok()?.checked_mul(multiplier)?;
            let denominator = 10u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
            if numerator % denominator != 0 {
                return None;
            }
            numerator / denominator
        };
        total = total.checked_add(whole.checked_mul(multiplier)?.checked_add(fraction)?)?;
        parts += 1;
    }
    if parts == 0 {
        return None;
    }
    let signed = i128::try_from(total).ok()?;
    i64::try_from(if negative { -signed } else { signed }).ok()
}
