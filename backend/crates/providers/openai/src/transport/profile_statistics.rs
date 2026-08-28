//! Codex Desktop 当前账号个人资料与累计统计 HTTP contract。

use chrono::NaiveDate;
use gateway_protocol::openai::events::retry_after_seconds_from_body;
use reqwest::StatusCode;
use serde::Deserialize;

use super::{
    CodexBackendClient, CodexClientError, CodexClientResult, CodexRequestContext,
    client::{
        CodexBackendTransport, CodexTransportMetrics, read_capped_response_body,
        retry_after_seconds,
    },
    diagnostics::CodexUpstreamSendPhase,
    endpoints::{WHAM_PROFILE_STATISTICS_PATH, endpoint_url},
    response_meta,
};

/// 单次 profile 响应允许保留和解析的最大字节数。
pub const MAX_CODEX_PROFILE_STATISTICS_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProfileStatisticsSummary {
    pub total_text_tokens: Option<u64>,
    pub peak_tokens: Option<u64>,
    pub longest_task_duration_ms: Option<u64>,
    pub current_streak_days: Option<u64>,
    pub longest_streak_days: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProfileDailyUsage {
    pub date: NaiveDate,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProfileInvocation {
    pub invocation_type: String,
    pub plugin_id: Option<String>,
    pub plugin_name: Option<String>,
    pub skill_id: Option<String>,
    pub skill_name: Option<String>,
    pub usage_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexProfileActivityInsights {
    pub fast_mode_percent: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub reasoning_effort_percent: Option<f64>,
    pub skills_explored: Option<u64>,
    pub total_skills_used: Option<u64>,
    pub total_threads: Option<u64>,
    pub invocations: Option<Vec<CodexProfileInvocation>>,
}

#[derive(Clone, PartialEq)]
pub struct CodexProfileStatistics {
    pub display_name: Option<String>,
    pub username: Option<String>,
    pub image_url: Option<String>,
    pub has_stats_error: bool,
    pub summary: CodexProfileStatisticsSummary,
    pub daily_usage: Option<Vec<CodexProfileDailyUsage>>,
    pub activity_insights: CodexProfileActivityInsights,
}

impl std::fmt::Debug for CodexProfileStatistics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexProfileStatistics")
            .field(
                "display_name",
                &self.display_name.as_ref().map(|_| "<redacted>"),
            )
            .field("username", &self.username.as_ref().map(|_| "<redacted>"))
            .field("image_url", &self.image_url.as_ref().map(|_| "<redacted>"))
            .field("has_stats_error", &self.has_stats_error)
            .field("summary", &"<redacted>")
            .field("daily_usage", &"<redacted>")
            .field("activity_insights", &"<redacted>")
            .finish()
    }
}

#[derive(Deserialize)]
struct ProfileStatisticsWire {
    profile: ProfileWire,
    stats: StatisticsWire,
    #[serde(default)]
    metadata: MetadataWire,
}

#[derive(Deserialize)]
struct ProfileWire {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    profile_picture_url: Option<String>,
}

#[derive(Deserialize)]
struct StatisticsWire {
    #[serde(default)]
    lifetime_tokens: Option<u64>,
    #[serde(default)]
    peak_daily_tokens: Option<u64>,
    #[serde(default)]
    longest_running_turn_sec: Option<u64>,
    #[serde(default)]
    current_streak_days: Option<u64>,
    #[serde(default)]
    longest_streak_days: Option<u64>,
    #[serde(default)]
    daily_usage_buckets: Option<Vec<DailyUsageWire>>,
    #[serde(default)]
    fast_mode_usage_percentage: Option<f64>,
    #[serde(default)]
    most_used_reasoning_effort: Option<String>,
    #[serde(default)]
    most_used_reasoning_effort_percentage: Option<f64>,
    #[serde(default)]
    unique_skills_used: Option<u64>,
    #[serde(default)]
    total_skills_used: Option<u64>,
    #[serde(default)]
    total_threads: Option<u64>,
    #[serde(default)]
    top_invocations: Option<Vec<InvocationWire>>,
}

#[derive(Deserialize)]
struct DailyUsageWire {
    start_date: NaiveDate,
    tokens: u64,
}

#[derive(Deserialize)]
struct InvocationWire {
    #[serde(rename = "type")]
    invocation_type: String,
    #[serde(default)]
    plugin_id: Option<String>,
    #[serde(default)]
    plugin_name: Option<String>,
    #[serde(default)]
    skill_id: Option<String>,
    #[serde(default)]
    skill_name: Option<String>,
    #[serde(default)]
    usage_count: Option<u64>,
}

#[derive(Default, Deserialize)]
struct MetadataWire {
    #[serde(default)]
    stats_error: Option<String>,
}

impl CodexBackendClient {
    pub async fn fetch_profile_statistics(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexProfileStatistics> {
        let response = self
            .client
            .get(endpoint_url(&self.base_url, WHAM_PROFILE_STATISTICS_PATH))
            .headers(self.usage_request_headers(context)?)
            .send()
            .await?;
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let body =
            read_capped_response_body(response, MAX_CODEX_PROFILE_STATISTICS_BODY_BYTES).await?;
        if body.limit_exceeded() {
            return Err(upstream_error(
                if status.is_success() {
                    StatusCode::BAD_GATEWAY
                } else {
                    status
                },
                "upstream profile-statistics response exceeded the body limit".to_owned(),
                retry_after_seconds,
                diagnostics,
            ));
        }
        let body = body.into_string();
        if !status.is_success() {
            return Err(upstream_error(
                status,
                body.clone(),
                retry_after_seconds.or_else(|| retry_after_seconds_from_body(&body)),
                diagnostics,
            ));
        }

        let wire = serde_json::from_str::<ProfileStatisticsWire>(&body).map_err(|_| {
            upstream_error(
                StatusCode::BAD_GATEWAY,
                "invalid profile-statistics response".to_owned(),
                None,
                diagnostics,
            )
        })?;
        Ok(profile_statistics(wire))
    }
}

fn profile_statistics(wire: ProfileStatisticsWire) -> CodexProfileStatistics {
    let longest_task_duration_ms = wire
        .stats
        .longest_running_turn_sec
        .map(|seconds| seconds.saturating_mul(1000));
    let mut daily_usage = wire.stats.daily_usage_buckets.map(|daily| {
        daily
            .into_iter()
            .map(|entry| CodexProfileDailyUsage {
                date: entry.start_date,
                tokens: entry.tokens,
            })
            .collect::<Vec<_>>()
    });
    if let Some(daily_usage) = &mut daily_usage {
        daily_usage.sort_by_key(|entry| entry.date);
    }
    let invocations = wire.stats.top_invocations.map(|invocations| {
        invocations
            .into_iter()
            .map(|invocation| CodexProfileInvocation {
                invocation_type: invocation.invocation_type.trim().to_owned(),
                plugin_id: trimmed(invocation.plugin_id),
                plugin_name: trimmed(invocation.plugin_name),
                skill_id: trimmed(invocation.skill_id),
                skill_name: trimmed(invocation.skill_name),
                usage_count: invocation.usage_count,
            })
            .collect()
    });
    CodexProfileStatistics {
        display_name: trimmed(wire.profile.display_name),
        username: trimmed(wire.profile.username),
        image_url: trimmed(wire.profile.profile_picture_url),
        has_stats_error: trimmed(wire.metadata.stats_error).is_some(),
        summary: CodexProfileStatisticsSummary {
            total_text_tokens: wire.stats.lifetime_tokens,
            peak_tokens: wire.stats.peak_daily_tokens,
            longest_task_duration_ms,
            current_streak_days: wire.stats.current_streak_days,
            longest_streak_days: wire.stats.longest_streak_days,
        },
        daily_usage,
        activity_insights: CodexProfileActivityInsights {
            fast_mode_percent: wire.stats.fast_mode_usage_percentage,
            reasoning_effort: trimmed(wire.stats.most_used_reasoning_effort),
            reasoning_effort_percent: wire.stats.most_used_reasoning_effort_percentage,
            skills_explored: wire.stats.unique_skills_used,
            total_skills_used: wire.stats.total_skills_used,
            total_threads: wire.stats.total_threads,
            invocations,
        },
    }
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn upstream_error(
    status: StatusCode,
    body: String,
    retry_after_seconds: Option<u64>,
    diagnostics: super::CodexUpstreamDiagnostics,
) -> CodexClientError {
    CodexClientError::Upstream {
        status,
        body,
        client_response: None,
        retry_after_seconds,
        diagnostics: Box::new(diagnostics),
        set_cookie_headers: Vec::new(),
        rate_limit_headers: Vec::new(),
        transport: CodexBackendTransport::HttpSse,
        transport_metrics: Box::<CodexTransportMetrics>::default(),
        send_phase: CodexUpstreamSendPhase::AfterPayload,
    }
}
