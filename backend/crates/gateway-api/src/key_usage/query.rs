//! 查询不接受 Key、账号或 Provider 范围；禁止未知字段穿透权限边界

use axum::http::StatusCode;
use chrono::Duration;
use gateway_admin::model::{
    PageSize,
    key_usage::{KeyUsageQuery, KeyUsageRecordKind, KeyUsageRecordsQuery},
};
use serde::Deserialize;

use crate::admin::AdminError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyQuery {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OverviewQuery {
    period: Option<String>,
    as_of: Option<i64>,
    start_time: Option<String>,
    end_time: Option<String>,
    model: Option<String>,
}

impl OverviewQuery {
    pub(super) fn into_domain(
        self,
        timezone: gateway_core::time::DeploymentTimeZone,
    ) -> Result<KeyUsageQuery, AdminError> {
        let range = crate::time::query_range(
            self.start_time.as_deref(),
            self.end_time.as_deref(),
            self.period.as_deref(),
            self.as_of,
            timezone,
            "today",
        )
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
    period: Option<String>,
    as_of: Option<i64>,
    start_time: Option<String>,
    end_time: Option<String>,
    model: Option<String>,
    current_page: Option<u32>,
    page_size: Option<u16>,
    #[serde(default)]
    kind: RecordKind,
}

impl RecordsQuery {
    pub(super) fn into_domain(
        self,
        timezone: gateway_core::time::DeploymentTimeZone,
    ) -> Result<KeyUsageRecordsQuery, AdminError> {
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
                period: self.period,
                as_of: self.as_of,
                start_time: self.start_time,
                end_time: self.end_time,
                model: self.model,
            }
            .into_domain(timezone)?,
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
