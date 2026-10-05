//! 部署时区的日历换算，持久化和比较始终使用 UTC 时间点

use std::str::FromStr;

use chrono::{DateTime, Days, NaiveDate, NaiveDateTime, Offset as _, TimeZone as _, Utc};
use chrono_tz::{GapInfo, Tz};
use serde::{Deserialize, Serialize};

/// 由组合根传递的不可变 IANA 时区
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct DeploymentTimeZone(Tz);

impl Default for DeploymentTimeZone {
    fn default() -> Self {
        Self(chrono_tz::Asia::Shanghai)
    }
}

impl FromStr for DeploymentTimeZone {
    type Err = chrono_tz::ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

impl DeploymentTimeZone {
    #[must_use]
    pub fn name(self) -> &'static str {
        self.0.name()
    }

    #[must_use]
    pub const fn iana(self) -> Tz {
        self.0
    }

    #[must_use]
    pub fn local(self, value: DateTime<Utc>) -> DateTime<Tz> {
        value.with_timezone(&self.0)
    }

    /// 重复本地时刻只取较早时间点，缺失时刻由调用方决定是否跳过
    #[must_use]
    pub fn resolve_local(self, value: NaiveDateTime) -> Option<DateTime<Utc>> {
        self.0
            .from_local_datetime(&value)
            .earliest()
            .map(|value| value.to_utc())
    }

    /// 午夜跳时取缺口结束点，包含整日跳过的历史时区变更
    #[must_use]
    pub fn date_start(self, date: NaiveDate) -> Option<DateTime<Utc>> {
        let midnight = date.and_hms_opt(0, 0, 0)?;
        self.resolve_local(midnight).or_else(|| {
            GapInfo::new(&midnight, &self.0)?
                .end
                .map(|value| value.to_utc())
        })
    }

    #[must_use]
    pub fn day_start(self, value: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.date_start(self.local_date(value)?)
    }

    /// 先移动本地日期再解析时间点，避免把自然日写成固定 24 小时
    #[must_use]
    pub fn days_before(self, value: DateTime<Utc>, days: u64) -> Option<DateTime<Utc>> {
        self.date_start(self.local_date(value)?.checked_sub_days(Days::new(days))?)
    }

    #[must_use]
    pub fn days_after(self, value: DateTime<Utc>, days: u64) -> Option<DateTime<Utc>> {
        self.date_start(self.local_date(value)?.checked_add_days(Days::new(days))?)
    }

    fn local_date(self, value: DateTime<Utc>) -> Option<NaiveDate> {
        let local = self.local(value);
        // UTC 可表示的端点在叠加时区偏移后仍可能超出本地日历范围
        local
            .naive_utc()
            .checked_add_offset(local.offset().fix())
            .map(|value| value.date())
    }
}
