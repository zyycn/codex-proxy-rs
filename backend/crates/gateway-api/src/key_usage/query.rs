//! 查询不接受 Key、账号或 Provider 范围；禁止未知字段穿透权限边界。

use axum::http::StatusCode;
use chrono::{DateTime, Duration};
use gateway_admin::model::{
    PageSize,
    key_usage::{KeyUsageQuery, KeyUsageRecordKind, KeyUsageRecordsQuery},
    observability::TimeRange,
};
use serde::Deserialize;

use crate::admin::AdminError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OverviewQuery {
    start_time: String,
    end_time: String,
    model: Option<String>,
}

impl OverviewQuery {
    pub(super) fn into_domain(self) -> Result<KeyUsageQuery, AdminError> {
        let parse = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .map(|value| value.to_utc())
                .map_err(|_| {
                    AdminError::invalid_request(
                        StatusCode::BAD_REQUEST,
                        "时间必须使用 RFC3339 格式",
                    )
                })
        };
        let range = TimeRange::new(parse(&self.start_time)?, parse(&self.end_time)?)
            .map_err(|_| AdminError::invalid_request(StatusCode::BAD_REQUEST, "时间范围不合法"))?;
        if range.end - range.start > Duration::days(31) {
            return Err(AdminError::invalid_request(
                StatusCode::BAD_REQUEST,
                "一次最多查询 31 天的用量",
            ));
        }
        let model = self
            .model
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if model
            .as_ref()
            .is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(AdminError::invalid_request(
                StatusCode::BAD_REQUEST,
                "模型筛选内容不合法",
            ));
        }
        Ok(KeyUsageQuery { range, model })
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
enum RecordKind {
    #[default]
    Success,
    Error,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RecordsQuery {
    start_time: String,
    end_time: String,
    model: Option<String>,
    current_page: Option<u32>,
    page_size: Option<u16>,
    #[serde(default)]
    kind: RecordKind,
}

impl RecordsQuery {
    pub(super) fn into_domain(self) -> Result<KeyUsageRecordsQuery, AdminError> {
        let current_page = self.current_page.unwrap_or(1);
        let page_size = self.page_size.unwrap_or(20);
        if current_page == 0 || !(1..=100).contains(&page_size) {
            return Err(AdminError::invalid_request(
                StatusCode::BAD_REQUEST,
                "页码必须大于 0，每页数量为 1–100",
            ));
        }
        Ok(KeyUsageRecordsQuery {
            usage: OverviewQuery {
                start_time: self.start_time,
                end_time: self.end_time,
                model: self.model,
            }
            .into_domain()?,
            kind: match self.kind {
                RecordKind::Success => KeyUsageRecordKind::Success,
                RecordKind::Error => KeyUsageRecordKind::Error,
            },
            current_page,
            page_size: PageSize::new(page_size).map_err(|_| {
                AdminError::invalid_request(StatusCode::BAD_REQUEST, "每页数量不合法")
            })?,
        })
    }
}
