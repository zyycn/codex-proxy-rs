//! 配额响应头与 token 用量写入的唯一 feature owner。

use serde_json::Value;

use crate::{
    dispatch::lifecycle::contract::{
        AttemptObservation, AttemptObservationKind, CompleteResponseFacts,
    },
    fleet::{
        pool::AccountPoolService,
        quota::{
            QuotaCreditsObservation, QuotaLimitObservation, QuotaObservation,
            QuotaWindowObservation,
        },
        usage::ResponseUsage,
    },
    upstream::openai::{
        protocol::{
            events::{RateLimitWindow, TokenUsage, parse_rate_limit_headers},
            responses::CodexResponsesRequest,
        },
        transport::{CodexRateLimitHeaderUpdates, CodexTurnStateUpdate},
    },
};

pub(super) struct UsageScope {
    image_generation_requested: bool,
}

pub(super) struct StreamUsageExit {
    pub(super) rate_limit_headers: Vec<(String, String)>,
    pub(super) turn_state: Option<String>,
}

pub(super) struct StreamExit<'a> {
    pub account_pool: &'a AccountPoolService,
    pub account_id: &'a str,
    pub account_plan_type: Option<&'a str>,
    pub rate_limit_headers: &'a [(String, String)],
    pub rate_limit_header_updates: Option<&'a CodexRateLimitHeaderUpdates>,
    pub turn_state_update: Option<&'a CodexTurnStateUpdate>,
    pub turn_state: Option<&'a str>,
    pub usage: Option<TokenUsage>,
}

pub(super) struct UsageController;

impl UsageController {
    pub(super) fn enter(request: &CodexResponsesRequest) -> UsageScope {
        UsageScope {
            image_generation_requested: request.expects_image_generation(),
        }
    }

    pub(super) async fn observe_attempt(
        account_pool: &AccountPoolService,
        scope: &UsageScope,
        observation: &AttemptObservation,
    ) {
        if !matches!(
            observation.kind,
            AttemptObservationKind::CompleteResponse(CompleteResponseFacts::Empty)
        ) {
            return;
        }
        let Some(account_id) = observation
            .account
            .as_ref()
            .map(|account| account.id.as_str())
        else {
            return;
        };
        account_pool
            .record_empty_response_attempt(account_id, scope.image_generation_requested)
            .await;
    }

    pub(super) async fn leave_complete(
        account_pool: &AccountPoolService,
        account_id: &str,
        usage: Option<TokenUsage>,
        image_generation_requested: bool,
    ) {
        if let Some(usage) = usage {
            account_pool
                .record_response_usage(
                    account_id,
                    response_usage(usage),
                    image_generation_requested,
                )
                .await;
        }
    }

    pub(super) async fn sync_passive_quota(
        account_pool: &AccountPoolService,
        account_id: &str,
        plan_type: Option<&str>,
        headers: &[(String, String)],
    ) {
        let Some(observation) = quota_observation(headers) else {
            return;
        };
        account_pool
            .sync_passive_quota_observation_for_account(account_id, plan_type, &observation)
            .await;
    }

    pub(super) async fn leave_stream(exit: StreamExit<'_>) -> StreamUsageExit {
        let mut rate_limit_headers = exit.rate_limit_headers.to_vec();
        if let Some(updates) = exit.rate_limit_header_updates {
            rate_limit_headers.extend(updates.lock().await.iter().cloned());
        }
        Self::sync_passive_quota(
            exit.account_pool,
            exit.account_id,
            exit.account_plan_type,
            &rate_limit_headers,
        )
        .await;
        let turn_state = if let Some(update) = exit.turn_state_update {
            update.lock().await.clone()
        } else {
            exit.turn_state.map(ToString::to_string)
        };
        if let Some(usage) = exit.usage {
            exit.account_pool
                .record_response_usage(exit.account_id, response_usage(usage), false)
                .await;
        }
        StreamUsageExit {
            rate_limit_headers,
            turn_state,
        }
    }
}

fn response_usage(usage: TokenUsage) -> ResponseUsage {
    ResponseUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        total_tokens: usage.total_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
    }
}

fn quota_observation(headers: &[(String, String)]) -> Option<QuotaObservation> {
    let parsed = parse_rate_limit_headers(headers)?;
    Some(QuotaObservation {
        limits: parsed
            .limits
            .into_iter()
            .map(|(limit_id, details)| {
                (
                    limit_id,
                    QuotaLimitObservation {
                        limit_id: details.limit_id,
                        limit_name: details.limit_name,
                        metered_feature: None,
                        allowed: details.allowed,
                        limit_reached: details.limit_reached,
                        primary: details.primary.map(quota_window_observation),
                        secondary: details.secondary.map(quota_window_observation),
                    },
                )
            })
            .collect(),
        active_limit: parsed.active_limit,
        credits: parsed.credits.map(|credits| QuotaCreditsObservation {
            has_credits: credits.has_credits,
            unlimited: credits.unlimited,
            overage_limit_reached: false,
            balance: credits.balance.map(Value::String),
        }),
        spend_control: None,
        plan_type: parsed.plan_type,
        promo_message: parsed.promo_message,
        rate_limit_reached_type: parsed.rate_limit_reached_type,
    })
}

fn quota_window_observation(window: RateLimitWindow) -> QuotaWindowObservation {
    QuotaWindowObservation {
        used_percent: window.used_percent,
        window_minutes: window.window_minutes,
        reset_at: window.reset_at,
        used: None,
        limit: None,
    }
}
