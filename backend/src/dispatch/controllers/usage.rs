//! 配额响应头与 token 用量写入的唯一 feature owner。

use crate::{
    dispatch::lifecycle::contract::{
        AttemptObservation, AttemptObservationKind, CompleteResponseFacts,
    },
    fleet::pool::AccountPoolService,
    upstream::openai::{
        protocol::{events::TokenUsage, responses::CodexResponsesRequest},
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
                .record_response_usage(account_id, usage, image_generation_requested)
                .await;
        }
    }

    pub(super) async fn leave_stream(exit: StreamExit<'_>) -> StreamUsageExit {
        let mut rate_limit_headers = exit.rate_limit_headers.to_vec();
        if let Some(updates) = exit.rate_limit_header_updates {
            rate_limit_headers.extend(updates.lock().await.iter().cloned());
        }
        exit.account_pool
            .sync_passive_rate_limit_headers_for_account(
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
                .record_token_usage(exit.account_id, &usage)
                .await;
        }
        StreamUsageExit {
            rate_limit_headers,
            turn_state,
        }
    }
}
