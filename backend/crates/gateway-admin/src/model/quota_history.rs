//! 单个额度桶的历史观测；原始文档只在宿主内交给 Provider 解释

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{observability::TimeRange, provider_credentials::ProviderDocument};

#[derive(Debug, Clone)]
pub struct QuotaHistoryQuery {
    pub window_key: String,
    pub range: TimeRange,
    pub cursor: Option<QuotaHistoryCursor>,
}

impl QuotaHistoryQuery {
    /// 起点基线最多回看一个查询区间
    pub fn scan_start(&self) -> DateTime<Utc> {
        self.range
            .start
            .checked_sub_signed(self.range.end - self.range.start)
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
    }
}

/// 完成时间可能相同，继续读取时保留数据库精度和请求 ID
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaHistoryCursor {
    pub completed_at: DateTime<Utc>,
    pub id: String,
}

pub struct QuotaDocumentPage {
    pub documents: Vec<(DateTime<Utc>, ProviderDocument)>,
    pub next_cursor: Option<QuotaHistoryCursor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaReading {
    pub plan_type: Option<String>,
    pub used_percent: f64,
    pub reset_at: Option<DateTime<Utc>>,
}

pub type QuotaHistoryParser = fn(&ProviderDocument, &str) -> Option<QuotaReading>;

pub struct QuotaHistoryPoint {
    pub observed_at: DateTime<Utc>,
    pub reading: QuotaReading,
}

#[derive(Default)]
pub struct QuotaHistoryPage {
    pub samples: Vec<QuotaHistoryPoint>,
    pub baseline: Option<QuotaHistoryPoint>,
    pub next_cursor: Option<QuotaHistoryCursor>,
}

impl QuotaHistoryPage {
    pub fn from_documents(
        page: QuotaDocumentPage,
        query: &QuotaHistoryQuery,
        parse: QuotaHistoryParser,
    ) -> Self {
        let mut result = Self {
            next_cursor: page.next_cursor,
            ..Self::default()
        };
        for (observed_at, document) in page.documents {
            let Some(reading) = parse(&document, &query.window_key) else {
                continue;
            };
            let point = QuotaHistoryPoint {
                observed_at,
                reading,
            };
            if observed_at < query.range.start {
                // 倒序扫描遇到的第一个同桶读数就是最近基线，无需再向前翻页
                result.baseline = Some(point);
                result.next_cursor = None;
                break;
            }
            result.samples.push(point);
        }
        result
    }
}
