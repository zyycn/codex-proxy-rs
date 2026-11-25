//! 客户端版本与账号资料的管理展示投影

use crate::profile::{CodexDesktopReleaseSnapshot, CodexWireProfile};
use crate::transport::{CodexProfileAvatar, CodexProfileStatistics};
use futures::TryStreamExt as _;
use gateway_admin::model::observability::{DashboardDesktopRelease, DesktopReleaseStatus};
use gateway_admin::model::provider_credentials::{
    ProviderProfileActivityInsights, ProviderProfileAvatar, ProviderProfileAvatarStreamError,
    ProviderProfileDailyUsage, ProviderProfileInvocation, ProviderProfileStatistics,
    ProviderProfileStatisticsSummary, ProviderResetCredit, ProviderResetCredits,
};

pub(super) fn dashboard_desktop_release(
    profile: &CodexWireProfile,
    snapshot: CodexDesktopReleaseSnapshot,
) -> DashboardDesktopRelease {
    let status = if snapshot.checked_at.is_none() {
        DesktopReleaseStatus::Unchecked
    } else if snapshot.last_error.is_some() {
        DesktopReleaseStatus::Failed
    } else if snapshot.latest.as_ref().is_some_and(|latest| {
        latest.version == profile.desktop_version && latest.build == profile.desktop_build
    }) {
        DesktopReleaseStatus::Current
    } else if snapshot.latest.is_some() {
        DesktopReleaseStatus::UpdateAvailable
    } else {
        DesktopReleaseStatus::Failed
    };
    let latest = snapshot.latest;
    DashboardDesktopRelease {
        status,
        checked_at: snapshot.checked_at,
        latest_version: latest.as_ref().map(|release| release.version.clone()),
        latest_build: latest.as_ref().map(|release| release.build.clone()),
        published_at: latest.as_ref().and_then(|release| release.published_at),
        minimum_system_version: latest
            .as_ref()
            .and_then(|release| release.minimum_system_version.clone()),
        hardware_requirements: latest
            .as_ref()
            .and_then(|release| release.hardware_requirements.clone()),
        download_url: latest
            .as_ref()
            .and_then(|release| release.download_url.clone()),
        download_size: latest.as_ref().and_then(|release| release.download_size),
        signature_present: latest.as_ref().map(|release| release.signature_present),
        error: snapshot.last_error,
    }
}
pub(super) fn project_profile_statistics(
    statistics: CodexProfileStatistics,
) -> ProviderProfileStatistics {
    ProviderProfileStatistics {
        display_name: statistics.display_name,
        username: statistics.username,
        image_url: statistics.image_url,
        has_stats_error: statistics.has_stats_error,
        summary: ProviderProfileStatisticsSummary {
            total_text_tokens: statistics.summary.total_text_tokens,
            peak_tokens: statistics.summary.peak_tokens,
            longest_task_duration_ms: statistics.summary.longest_task_duration_ms,
            current_streak_days: statistics.summary.current_streak_days,
            longest_streak_days: statistics.summary.longest_streak_days,
        },
        daily_usage: statistics.daily_usage.map(|daily| {
            daily
                .into_iter()
                .map(|entry| ProviderProfileDailyUsage {
                    date: entry.date,
                    tokens: entry.tokens,
                })
                .collect()
        }),
        activity_insights: ProviderProfileActivityInsights {
            fast_mode_percent: statistics.activity_insights.fast_mode_percent,
            reasoning_effort: statistics.activity_insights.reasoning_effort,
            reasoning_effort_percent: statistics.activity_insights.reasoning_effort_percent,
            skills_explored: statistics.activity_insights.skills_explored,
            total_skills_used: statistics.activity_insights.total_skills_used,
            total_threads: statistics.activity_insights.total_threads,
            invocations: statistics.activity_insights.invocations.map(|invocations| {
                invocations
                    .into_iter()
                    .map(|invocation| ProviderProfileInvocation {
                        invocation_type: invocation.invocation_type,
                        plugin_id: invocation.plugin_id,
                        plugin_name: invocation.plugin_name,
                        skill_id: invocation.skill_id,
                        skill_name: invocation.skill_name,
                        usage_count: invocation.usage_count,
                    })
                    .collect()
            }),
        },
    }
}

pub(super) fn project_profile_avatar(avatar: CodexProfileAvatar) -> ProviderProfileAvatar {
    ProviderProfileAvatar {
        content_type: avatar.content_type,
        content_length: avatar.content_length,
        etag: avatar.etag,
        body: Box::pin(avatar.body.map_err(|_| ProviderProfileAvatarStreamError)),
    }
}

pub(super) fn project_reset_credits(
    credits: crate::transport::CodexRateLimitResetCredits,
) -> ProviderResetCredits {
    ProviderResetCredits {
        available_count: credits.available_count,
        credits: credits
            .credits
            .into_iter()
            .map(project_reset_credit)
            .collect(),
    }
}

pub(super) fn project_reset_credit(
    credit: crate::transport::CodexRateLimitResetCredit,
) -> ProviderResetCredit {
    ProviderResetCredit {
        id: credit.id,
        status: credit.status,
        title: credit.title,
        expires_at: credit.expires_at,
        reset_type: credit.reset_type,
    }
}
