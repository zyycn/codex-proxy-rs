//! 智能调度参数；固定十分位保证快照可精确比较，浮点数只用于 wire 与评分

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(into = "SmartSchedulingValues")]
pub struct SmartSchedulingConfig {
    weights: [u8; 6],
}

impl<'de> Deserialize<'de> for SmartSchedulingConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // flatten 会缓冲字段，arbitrary_precision 的小数需先恢复为 JSON Number
        // 随后仍由同一类型检查完整字段、数值范围和十分位精度
        let value = serde_json::Value::deserialize(deserializer)?;
        let values = serde_json::from_value::<SmartSchedulingValues>(value)
            .map_err(serde::de::Error::custom)?;
        Self::try_from(values).map_err(serde::de::Error::custom)
    }
}

impl Default for SmartSchedulingConfig {
    fn default() -> Self {
        Self {
            weights: [10, 8, 10, 5, 0, 0],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SmartSchedulingConfigError {
    #[error("{0} must be between 0 and 10 with at most one decimal place")]
    InvalidWeight(&'static str),
    #[error("at least one smart scheduling weight must be positive")]
    EmptyWeights,
}

impl SmartSchedulingConfig {
    /// 顺序为负载、剩余额度、健康、首输出延迟、额度重置和排队压力
    pub fn new(weights: [f64; 6]) -> Result<Self, SmartSchedulingConfigError> {
        let mut tenths = [0; 6];
        for (index, field) in [
            "loadWeight",
            "quotaWeight",
            "healthWeight",
            "latencyWeight",
            "resetWeight",
            "queueWeight",
        ]
        .into_iter()
        .enumerate()
        {
            let value = weights[index];
            if !value.is_finite()
                || !(0.0..=10.0).contains(&value)
                || (value * 10.0).round() / 10.0 != value
            {
                return Err(SmartSchedulingConfigError::InvalidWeight(field));
            }
            tenths[index] = (value * 10.0).round() as u8;
        }
        if tenths == [0; 6] {
            return Err(SmartSchedulingConfigError::EmptyWeights);
        }
        Ok(Self { weights: tenths })
    }

    #[must_use]
    pub fn weights(self) -> [f64; 6] {
        self.weights.map(|weight| f64::from(weight) / 10.0)
    }

    pub(crate) fn score_tolerance(self) -> f64 {
        // 与系数同步缩放，避免仅放大相同比例的系数就改变近似最优候选集合
        // 排队系数只参与入队选择，不能放大立即选号的候选容差
        0.05 * f64::from(
            self.weights[..5]
                .iter()
                .copied()
                .map(u16::from)
                .sum::<u16>(),
        ) / 33.0
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SmartSchedulingValues {
    load_weight: f64,
    quota_weight: f64,
    health_weight: f64,
    latency_weight: f64,
    reset_weight: f64,
    queue_weight: f64,
}

impl TryFrom<SmartSchedulingValues> for SmartSchedulingConfig {
    type Error = SmartSchedulingConfigError;

    fn try_from(value: SmartSchedulingValues) -> Result<Self, Self::Error> {
        Self::new([
            value.load_weight,
            value.quota_weight,
            value.health_weight,
            value.latency_weight,
            value.reset_weight,
            value.queue_weight,
        ])
    }
}

impl From<SmartSchedulingConfig> for SmartSchedulingValues {
    fn from(value: SmartSchedulingConfig) -> Self {
        let [
            load_weight,
            quota_weight,
            health_weight,
            latency_weight,
            reset_weight,
            queue_weight,
        ] = value.weights();
        Self {
            load_weight,
            quota_weight,
            health_weight,
            latency_weight,
            reset_weight,
            queue_weight,
        }
    }
}
