//! 管理与自助页面共用的时间展示投影

use chrono::{DateTime, Utc};
use gateway_core::time::DeploymentTimeZone;

pub(crate) fn query_range(
    start: Option<&str>,
    end: Option<&str>,
    period: Option<&str>,
    as_of: Option<i64>,
    timezone: DeploymentTimeZone,
    default_period: &str,
) -> Result<gateway_admin::model::observability::TimeRange, crate::admin::WireValidationError> {
    use crate::admin::{WireValidationError, observability::parse_datetime};
    use gateway_admin::model::observability::{CalendarPeriod, TimeRange};
    if start.is_some() || end.is_some() {
        if period.is_some() || as_of.is_some() {
            return Err(WireValidationError::new("timeRange"));
        }
        let end = parse_datetime(end)?.unwrap_or_else(Utc::now);
        let start = match parse_datetime(start)? {
            Some(start) => start,
            None => {
                let period = CalendarPeriod::parse(default_period)
                    .map_err(|_| WireValidationError::new("period"))?;
                return TimeRange::calendar_at(period, end, timezone)
                    .map_err(|_| WireValidationError::new("timeRange"));
            }
        };
        return TimeRange::new(start, end).map_err(|_| WireValidationError::new("timeRange"));
    }
    let end = match as_of {
        Some(value) => DateTime::from_timestamp_millis(value)
            .ok_or_else(|| WireValidationError::new("asOf"))?,
        None => Utc::now(),
    };
    let period = CalendarPeriod::parse(period.unwrap_or(default_period))
        .map_err(|_| WireValidationError::new("period"))?;
    TimeRange::calendar_at(period, end, timezone).map_err(|_| WireValidationError::new("timeRange"))
}

#[derive(Clone, Copy)]
pub struct TimePresenter {
    timezone: DeploymentTimeZone,
    now: DateTime<Utc>,
}

impl TimePresenter {
    pub(crate) fn today(self) -> chrono::NaiveDate {
        self.timezone.local(self.now).date_naive()
    }
    pub(crate) fn now(self) -> DateTime<Utc> {
        self.now
    }
    pub(crate) fn rfc_display(self, value: Option<&str>) -> Option<String> {
        value
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| self.datetime(&value.to_utc()))
    }
    pub(crate) fn request_buckets(
        self,
        buckets: impl Iterator<Item = (DateTime<Utc>, u64)>,
    ) -> Vec<RequestBucketView> {
        buckets
            .map(|(start, count)| {
                let end = start + chrono::Duration::hours(1);
                RequestBucketView {
                    bucket_start: start,
                    request_count: count,
                    label: format!(
                        "{}–{} · {count} 次请求",
                        self.label(start, "%m-%d %H:%M"),
                        self.label(end, "%H:%M")
                    ),
                }
            })
            .collect()
    }

    #[must_use]
    pub fn new(timezone: DeploymentTimeZone) -> Self {
        Self {
            timezone,
            now: Utc::now(),
        }
    }

    pub(crate) fn datetime(self, value: &DateTime<Utc>) -> String {
        self.label(*value, "%Y-%m-%d %H:%M:%S")
    }

    pub(crate) fn time(self, value: &DateTime<Utc>) -> String {
        self.label(*value, "%H:%M:%S")
    }

    pub(crate) fn label(self, value: DateTime<Utc>, format: &str) -> String {
        self.timezone.local(value).format(format).to_string()
    }

    pub(crate) fn rfc3339(self, value: &DateTime<Utc>) -> String {
        self.timezone.local(*value).to_rfc3339()
    }

    pub(crate) fn relative(self, value: DateTime<Utc>, now: DateTime<Utc>) -> String {
        let elapsed = now.signed_duration_since(value);
        if elapsed.num_seconds() < 0 {
            return self.datetime(&value);
        }
        if elapsed.num_seconds() < 60 {
            return "刚刚".to_owned();
        }
        if elapsed.num_minutes() < 60 {
            return format!("{} 分钟前", elapsed.num_minutes());
        }
        if elapsed.num_hours() < 24 {
            return format!("{} 小时前", elapsed.num_hours());
        }
        format!("{} 天前", elapsed.num_days())
    }

    pub(crate) fn relative_optional(
        self,
        value: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> String {
        value.map_or_else(|| "—".to_owned(), |value| self.relative(value, now))
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestBucketView {
    pub(crate) bucket_start: DateTime<Utc>,
    pub(crate) request_count: u64,
    pub(crate) label: String,
}
