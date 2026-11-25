//! 观测指标的均值与比率计算

pub(super) fn average(sum: u64, count: u64) -> Option<u64> {
    (count > 0).then(|| sum / count)
}

pub(super) fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

pub(super) fn rate_or_zero(numerator: u64, denominator: u64) -> f64 {
    ratio(numerator, denominator).unwrap_or(0.0)
}
