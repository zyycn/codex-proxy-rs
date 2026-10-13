//! 可合并的固定对数直方图与连续样本秩分位数

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::super::{StoreResult, invalid};

pub(super) const HISTOGRAM_VERSION: i32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct Histogram {
    pub count: u64,
    pub sum: f64,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub(super) zero: u64,
    pub(super) bins: BTreeMap<i32, u64>,
}

impl Histogram {
    pub fn add(&mut self, value: f64) -> StoreResult<()> {
        if !value.is_finite() || value < 0.0 {
            return Err(invalid("analytics sample must be finite and nonnegative"));
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("histogram count overflow"))?;
        self.sum += value;
        self.minimum = Some(self.minimum.map_or(value, |old| old.min(value)));
        self.maximum = Some(self.maximum.map_or(value, |old| old.max(value)));
        if value == 0.0 {
            self.zero += 1;
        } else {
            let index = (value.log2() * 16.0).floor() as i32;
            *self.bins.entry(index).or_default() += 1;
        }
        Ok(())
    }

    pub fn merge(&mut self, other: &Self) -> StoreResult<()> {
        self.count = self
            .count
            .checked_add(other.count)
            .ok_or_else(|| invalid("histogram count overflow"))?;
        self.zero = self
            .zero
            .checked_add(other.zero)
            .ok_or_else(|| invalid("histogram count overflow"))?;
        self.sum += other.sum;
        if !self.sum.is_finite() {
            return Err(invalid("histogram sum overflow"));
        }
        if let Some(value) = other.minimum {
            self.minimum = Some(self.minimum.map_or(value, |old| old.min(value)));
        }
        if let Some(value) = other.maximum {
            self.maximum = Some(self.maximum.map_or(value, |old| old.max(value)));
        }
        for (index, count) in &other.bins {
            let current = self.bins.entry(*index).or_default();
            *current = current
                .checked_add(*count)
                .ok_or_else(|| invalid("histogram bin overflow"))?;
        }
        Ok(())
    }

    pub fn percentile(&self, fraction: f64) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let rank = self.count.saturating_sub(1) as f64 * fraction;
        let lower = rank.floor() as u64;
        let upper = rank.ceil() as u64;
        let a = self.at_rank(lower)?;
        let b = self.at_rank(upper)?;
        Some(a + (b - a) * rank.fract())
    }

    fn at_rank(&self, rank: u64) -> Option<f64> {
        if rank < self.zero {
            return Some(0.0);
        }
        let mut cumulative = self.zero;
        for (index, count) in &self.bins {
            cumulative += count;
            if rank < cumulative {
                let low = 2_f64.powf(f64::from(*index) / 16.0);
                let high = 2_f64.powf(f64::from(*index + 1) / 16.0);
                return Some((low + high) / 2.0);
            }
        }
        None
    }
}
