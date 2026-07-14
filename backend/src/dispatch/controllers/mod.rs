//! v1 请求生命周期控制器集合。
//!
//! 控制器集合是静态类型的 feature owner 边界：生命周期只依赖这里暴露的
//! typed hook，不直接知道具体功能名称或状态存储实现。

use std::{
    collections::BTreeSet,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use tokio::time::timeout;

use crate::{
    dispatch::{
        affinity::{
            AccountIdentityScope, AccountIdentityService, AccountScopedRequest,
            SessionAffinityService,
        },
        errors::{ClientFailure, ResponseDispatchError, upstream_error_set_cookie_headers},
        lifecycle::trace::{ResponseDispatchAttempt, ResponseDispatchTrace},
    },
    fleet::{account::Account, pool::AccountLease},
    models::service::ModelService,
    telemetry::recorder::Recorder,
    upstream::openai::protocol::events::TokenUsage,
    upstream::openai::transport::{
        CodexBackendClient, CodexBackendResponse, CodexBackendTransport, CodexClientError,
        CodexRateLimitHeaderUpdates, CodexResponseMetadata, CodexTurnStateUpdate,
        CodexUpstreamDiagnostics, WebSocketPoolDecision,
    },
};
use crate::{
    dispatch::{
        controllers::cloudflare::CloudflareRecovery,
        controllers::cyber_policy::{
            ClassifiedCyberPolicyFailure, CyberPolicyController, CyberPolicyScope,
        },
        controllers::{
            account_failure::AccountFailureController, history::HistoryController,
            history::HistoryState, quota::QuotaController,
        },
        lifecycle::{
            contract::{
                AttemptDecision, AttemptObservation, AttemptObservationKind, AttemptReturnKind,
                AttemptRoutingFacts, FinalOutcome,
            },
            stream::StreamTerminal,
        },
        transport::observation::UpstreamFailureFacts,
    },
    fleet::pool::AccountPoolService,
    upstream::openai::protocol::responses::{CodexResponsesRequest, ResponsesSseFailure},
};

mod account_failure;
mod account_state;
mod affinity;
pub(crate) mod cloudflare;
pub(crate) mod cyber_policy;
pub(in crate::dispatch) mod history;
mod quota;
pub(in crate::dispatch) mod telemetry;
mod usage;

const BEST_EFFORT_IO_TIMEOUT: Duration = Duration::from_millis(100);

pub(in crate::dispatch) struct CompleteExit<'a> {
    pub request: &'a CodexResponsesRequest,
    pub account_id: &'a str,
    pub body: &'a str,
    pub turn_state: Option<String>,
    pub usage: Option<TokenUsage>,
    pub image_generation_requested: bool,
    pub completed: bool,
    pub recorder: &'a Recorder,
    pub models: &'a ModelService,
    pub request_id: &'a str,
    pub route: &'a str,
    pub requested_model: &'a str,
    pub display_model: &'a str,
    pub started_at: Instant,
    pub response_id: Option<&'a str>,
    pub response: &'a CodexBackendResponse,
    pub trace: &'a ResponseDispatchTrace,
    pub attempt: &'a ResponseDispatchAttempt,
}

/// 已提交 stream 在 controller 洋葱退出阶段消费的稳定事实。
///
/// 该上下文不包含 live transport 执行器、账号 lease 或 shutdown 控制器；各 feature
/// owner 只能通过 `ControllerSet` 分发的窄 typed exit 读取所需字段。
pub(in crate::dispatch) struct StreamControllerContext {
    pub account_id: String,
    pub account_plan_type: Option<String>,
    pub request_id: String,
    pub route: String,
    pub display_model: String,
    pub requested_model: String,
    pub request: CodexResponsesRequest,
    pub transport: CodexBackendTransport,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub rate_limit_header_updates: Option<CodexRateLimitHeaderUpdates>,
    pub turn_state_update: Option<CodexTurnStateUpdate>,
    pub websocket_pool_decision: Option<WebSocketPoolDecision>,
    pub turn_state: Option<String>,
    pub diagnostics: CodexUpstreamDiagnostics,
    pub response_metadata: CodexResponseMetadata,
    pub transport_metrics: crate::upstream::openai::transport::CodexTransportMetrics,
    pub connection_local_continuation: bool,
    pub attempt: ResponseDispatchAttempt,
    pub attempts: Vec<ResponseDispatchAttempt>,
    pub started_at: Instant,
}

/// 单次 attempt 的原始上游失败上下文。
///
/// 生命周期只负责把事实交给控制器集合；Cloudflare 状态与遥测记录分别由各自
/// controller owner 消费，避免失败信息在最终 exhaustion 折叠时丢失。
pub(in crate::dispatch) struct AttemptUpstreamErrorContext<'a> {
    pub request_id: &'a str,
    pub account: &'a Account,
    pub route: &'a str,
    pub model: &'a str,
    pub started_at: Instant,
    pub stream: bool,
    pub transport: crate::upstream::openai::transport::CodexBackendTransport,
    pub request: &'a CodexResponsesRequest,
    pub error: &'a CodexClientError,
    pub trace: &'a ResponseDispatchTrace,
    pub attempt: &'a ResponseDispatchAttempt,
}

/// 请求在产生最终 dispatch error 后交给 telemetry owner 的稳定事实。
pub(in crate::dispatch) struct DispatchErrorObservation<'a> {
    pub request_id: &'a str,
    pub client_api_key_id: Option<&'a str>,
    pub account_id: Option<&'a str>,
    pub route: &'a str,
    pub model: &'a str,
    pub started_at: Instant,
    pub stream: bool,
    pub compact: bool,
    pub transport: Option<&'a str>,
}

/// commit 前 stream failure 的 telemetry 事实；client status 已包含在 typed error 中。
pub(in crate::dispatch) struct PrefetchedStreamFailureObservation<'a> {
    pub request_id: &'a str,
    pub account_id: &'a str,
    pub route: &'a str,
    pub model: &'a str,
    pub requested_model: &'a str,
    pub started_at: Instant,
    pub transport: CodexBackendTransport,
    pub request: &'a CodexResponsesRequest,
    pub failure: &'a ResponsesSseFailure,
    pub error: &'a ResponseDispatchError,
    pub diagnostics: &'a CodexUpstreamDiagnostics,
    pub rate_limit_headers: &'a [(String, String)],
    pub prefetched: &'a [u8],
    pub trace: &'a ResponseDispatchTrace,
    pub attempt: &'a ResponseDispatchAttempt,
}

/// 所有 controller owner 共同消费的失败事实。
///
/// attempt 与 commit 后的 stream finalizer 都必须先归一为该类型，避免同一种
/// 上游失败因发生阶段不同而漏掉 feature 副作用。
#[derive(Clone, Copy)]
enum ControllerFailureFact<'a> {
    Upstream(&'a UpstreamFailureFacts),
    Response(&'a ResponsesSseFailure),
}

#[derive(Clone, Copy)]
enum FailureObservation<'a> {
    Attempt(&'a AttemptObservation),
    Stream {
        account_id: &'a str,
        failure: &'a ResponsesSseFailure,
    },
}

impl<'a> FailureObservation<'a> {
    fn fact(self) -> Option<ControllerFailureFact<'a>> {
        match self {
            Self::Attempt(observation) => ControllerFailureFact::from_attempt(observation),
            Self::Stream { failure, .. } => Some(ControllerFailureFact::Response(failure)),
        }
    }
}

enum SharedFailureClassification<'a> {
    CyberPolicy(ClassifiedCyberPolicyFailure<'a>),
    AccountFailure(account_failure::ClassifiedFailure),
    Quota(quota::ClassifiedQuotaFailure),
}

impl<'a> ControllerFailureFact<'a> {
    fn from_attempt(observation: &'a AttemptObservation) -> Option<Self> {
        match &observation.kind {
            AttemptObservationKind::UpstreamFailure(facts) => Some(Self::Upstream(facts)),
            AttemptObservationKind::CompleteResponse(
                crate::dispatch::lifecycle::contract::CompleteResponseFacts::Failed(failure),
            )
            | AttemptObservationKind::StreamFailure(failure) => Some(Self::Response(failure)),
            _ => None,
        }
    }
}

/// 单次 attempt 的唯一分类结果；副作用与 decision 必须共同消费这个值。
enum AttemptClassification<'a> {
    Shared(SharedFailureClassification<'a>),
    Cloudflare(cloudflare::CloudflareFailure),
    History(AttemptDecision),
    Default(AttemptDecision),
}

struct StreamFailureClassification<'a> {
    failure: &'a ResponsesSseFailure,
    owner: Option<SharedFailureClassification<'a>>,
}

impl StreamFailureClassification<'_> {
    fn client_failure(&self) -> ClientFailure {
        match &self.owner {
            Some(SharedFailureClassification::CyberPolicy(classified)) => {
                CyberPolicyController::client_failure(classified)
            }
            Some(SharedFailureClassification::AccountFailure(classified)) => {
                AccountFailureController::client_failure(classified, self.failure)
            }
            Some(SharedFailureClassification::Quota(_)) => {
                QuotaController::client_failure(self.failure)
            }
            None => default_response_client_failure(self.failure),
        }
    }
}

/// 请求级控制器集合。使用具体类型而非动态 middleware 链，保证调用顺序和所有权可见。
#[derive(Clone)]
pub(in crate::dispatch) struct ControllerSet {
    cyber_policy: CyberPolicyController,
    session_affinity: Arc<SessionAffinityService>,
    account_pool: Arc<AccountPoolService>,
    codex: Arc<CodexBackendClient>,
    cloudflare: CloudflareRecovery,
    recorder: Arc<Recorder>,
}

/// 控制器在单个请求中的不可变状态快照。
pub(in crate::dispatch) struct ControllerRequestScope {
    cyber_policy: CyberPolicyScope,
    history: HistoryState,
    identity: AccountIdentityScope,
    usage: usage::UsageScope,
}

/// 请求 enter 阶段的控制器结果。
pub(in crate::dispatch) struct ControllerEnter {
    pub scope: ControllerRequestScope,
    pub preferred_account_id: Option<String>,
    pub excluded_account_ids: BTreeSet<String>,
}

pub(in crate::dispatch) struct AttemptAccountPreparationContext<'a> {
    pub codex: &'a CodexBackendClient,
    pub account_identity: &'a AccountIdentityService,
    pub request_id: &'a str,
}

pub(in crate::dispatch) struct PreparedAttemptAccount {
    pub lease: AccountLease,
    pub cookie_header: Option<String>,
}

pub(in crate::dispatch) enum AttemptAccountPreparation {
    Ready(Box<PreparedAttemptAccount>),
    Rejected,
}

pub(in crate::dispatch) struct AttemptRoutePreparationContext<'a> {
    pub account_identity: &'a AccountIdentityService,
    pub account_id: &'a str,
}

pub(in crate::dispatch) enum AttemptRoutePreparation {
    Ready(Box<AccountScopedRequest>),
    Unavailable { message: String },
}

impl ControllerSet {
    pub(in crate::dispatch) async fn prepare_attempt_account(
        &self,
        context: AttemptAccountPreparationContext<'_>,
        acquired: AccountLease,
    ) -> AttemptAccountPreparation {
        let usage_cookie = self
            .cloudflare
            .cookie_header_for_request(&acquired.account.id, "/codex/usage")
            .await;
        let acquired = match QuotaController::enter(
            quota::QuotaEnterContext {
                account_pool: &self.account_pool,
                codex: context.codex,
                cookie_header: usage_cookie.as_deref(),
                account_identity: context.account_identity,
                request_id: context.request_id,
            },
            acquired,
        )
        .await
        {
            quota::QuotaEnterOutcome::Ready(acquired) => *acquired,
            quota::QuotaEnterOutcome::LimitReached => {
                return AttemptAccountPreparation::Rejected;
            }
        };
        let cookie_header = self
            .cloudflare
            .cookie_header_for_request(&acquired.account.id, "/codex/responses")
            .await;
        AttemptAccountPreparation::Ready(Box::new(PreparedAttemptAccount {
            lease: acquired,
            cookie_header,
        }))
    }

    pub(in crate::dispatch) fn prepare_attempt_route(
        &self,
        scope: &mut ControllerRequestScope,
        request: &CodexResponsesRequest,
        context: AttemptRoutePreparationContext<'_>,
    ) -> AttemptRoutePreparation {
        let request = match HistoryController::prepare_attempt(
            &mut scope.history,
            request,
            context.account_id,
        ) {
            Ok(request) => request,
            Err(message) => return AttemptRoutePreparation::Unavailable { message },
        };
        AttemptRoutePreparation::Ready(Box::new(context.account_identity.scope_request(
            &mut scope.identity,
            request,
            context.account_id,
        )))
    }

    pub(in crate::dispatch) fn prepare_same_account_retry(
        &self,
        scope: &mut ControllerRequestScope,
        account_id: &str,
    ) -> bool {
        HistoryController::prepare_same_account_retry(&mut scope.history, account_id)
    }

    pub(in crate::dispatch) fn attempt_routing_facts(
        &self,
        scope: &ControllerRequestScope,
        account_id: Option<&str>,
    ) -> AttemptRoutingFacts {
        HistoryController::routing_facts(&scope.history, account_id)
    }

    pub(in crate::dispatch) async fn observe_complete_upstream(
        &self,
        account: &Account,
        set_cookie_headers: &[String],
        rate_limit_headers: &[(String, String)],
    ) {
        let _ = best_effort_controller_io(
            "cookie.complete",
            self.cloudflare
                .capture_set_cookie_headers(&account.id, set_cookie_headers),
        )
        .await;
        let _ = best_effort_controller_io(
            "quota.complete_headers",
            self.account_pool
                .sync_passive_rate_limit_headers(account, rate_limit_headers),
        )
        .await;
    }

    pub(in crate::dispatch) async fn observe_upstream_error(
        &self,
        context: AttemptUpstreamErrorContext<'_>,
    ) {
        let _ = best_effort_controller_io(
            "cookie.attempt_error",
            self.cloudflare.capture_set_cookie_headers(
                &context.account.id,
                upstream_error_set_cookie_headers(context.error),
            ),
        )
        .await;
        let _ = best_effort_controller_io(
            "telemetry.attempt_error",
            telemetry::TelemetryController::observe_upstream_error(&self.recorder, &context),
        )
        .await;
    }

    pub(in crate::dispatch) async fn observe_dispatch_error(
        &self,
        observation: DispatchErrorObservation<'_>,
        error: &ResponseDispatchError,
    ) {
        let _ = best_effort_controller_io(
            "telemetry.dispatch_error",
            telemetry::TelemetryController::observe_dispatch_error(
                &self.recorder,
                observation,
                error,
            ),
        )
        .await;
    }

    pub(in crate::dispatch) async fn observe_prefetched_stream_failure(
        &self,
        observation: PrefetchedStreamFailureObservation<'_>,
    ) {
        let _ = best_effort_controller_io(
            "telemetry.prefetched_failure",
            telemetry::TelemetryController::observe_prefetched_stream_failure(
                &self.recorder,
                observation,
            ),
        )
        .await;
    }

    pub(in crate::dispatch) async fn finalize_complete(
        &self,
        scope: ControllerRequestScope,
        exit: CompleteExit<'_>,
        outcome: FinalOutcome,
    ) {
        let successful = matches!(outcome, FinalOutcome::Completed | FinalOutcome::Incomplete);
        if successful {
            let _ = best_effort_controller_io(
                "cloudflare.complete",
                cloudflare::CloudflareController::leave_complete(&self.cloudflare, exit.account_id),
            )
            .await;
        }

        if successful {
            let _ = best_effort_controller_io(
                "usage.complete",
                usage::UsageController::leave_complete(
                    &self.account_pool,
                    exit.account_id,
                    exit.usage,
                    exit.image_generation_requested,
                ),
            )
            .await;
        }
        if exit.completed {
            let conversation_id = HistoryController::conversation_id(&scope.history, exit.request);
            let continuation_scope = HistoryController::continuation_scope(
                exit.request,
                exit.response.transport,
                exit.response.connection_local_continuation,
            );
            let _ = best_effort_controller_io(
                "affinity.complete",
                affinity::AffinityController::leave_complete(affinity::ResponseExit {
                    affinity: &self.session_affinity,
                    conversation_id,
                    request: exit.request,
                    account_id: exit.account_id,
                    body: exit.body,
                    turn_state: exit.turn_state.clone(),
                    usage: exit.usage,
                    continuation_scope,
                }),
            )
            .await;
        }
        if successful {
            let _ = best_effort_controller_io(
                "telemetry.complete",
                telemetry::TelemetryController::leave_complete(&exit),
            )
            .await;
        }
        exit.models
            .observe_models_etag(exit.response.response_metadata.models_etag.as_deref());
    }

    pub(in crate::dispatch) fn new(
        session_affinity: Arc<crate::dispatch::affinity::SessionAffinityService>,
        account_pool: Arc<AccountPoolService>,
        codex: Arc<CodexBackendClient>,
        cloudflare: CloudflareRecovery,
        recorder: Arc<Recorder>,
    ) -> Self {
        Self {
            cyber_policy: CyberPolicyController::new(Arc::clone(&session_affinity)),
            session_affinity,
            account_pool,
            codex,
            cloudflare,
            recorder,
        }
    }

    pub(in crate::dispatch) async fn enter(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> ControllerEnter {
        let usage = usage::UsageController::enter(request);
        let (history, cyber_policy, affinity_preferred_account_id) = tokio::join!(
            HistoryController::enter(&self.session_affinity, request),
            self.cyber_policy.prepare(request),
            affinity::AffinityController::preferred_account_id(
                &self.session_affinity,
                request,
                now,
            ),
        );
        let preferred_account_id = HistoryController::preferred_account_id(&history)
            .map(str::to_owned)
            .or(affinity_preferred_account_id);
        let excluded_account_ids = cyber_policy.excluded_account_ids();
        ControllerEnter {
            scope: ControllerRequestScope {
                cyber_policy,
                history,
                identity: AccountIdentityScope::new(preferred_account_id.clone()),
                usage,
            },
            preferred_account_id,
            excluded_account_ids,
        }
    }

    async fn finalize_stream_success(
        &self,
        scope: &ControllerRequestScope,
        terminal: &StreamTerminal,
    ) {
        match terminal {
            StreamTerminal::Completed { response } | StreamTerminal::Incomplete { response }
                if response.is_object() =>
            {
                self.cyber_policy.observe_success(&scope.cyber_policy).await
            }
            StreamTerminal::Completed { .. }
            | StreamTerminal::Incomplete { .. }
            | StreamTerminal::Failed { .. }
            | StreamTerminal::UpstreamClosed
            | StreamTerminal::UpstreamError { .. }
            | StreamTerminal::ProtocolError { .. }
            | StreamTerminal::CaptureLimitExceeded
            | StreamTerminal::Cancelled
            | StreamTerminal::DownstreamClosed
            | StreamTerminal::Shutdown => {}
        }
    }

    fn classify_stream_failure<'a>(
        &self,
        account_id: &'a str,
        failure: &'a ResponsesSseFailure,
    ) -> StreamFailureClassification<'a> {
        StreamFailureClassification {
            failure,
            owner: Self::classify_shared_failure(FailureObservation::Stream {
                account_id,
                failure,
            }),
        }
    }

    async fn apply_stream_account_state(&self, classification: &StreamFailureClassification<'_>) {
        match classification.owner.as_ref() {
            Some(SharedFailureClassification::AccountFailure(classified)) => {
                AccountFailureController::apply_effect(&self.account_pool, &self.codex, classified)
                    .await;
            }
            Some(SharedFailureClassification::Quota(classified)) => {
                QuotaController::apply_effect(&self.account_pool, &self.codex, classified).await;
            }
            Some(SharedFailureClassification::CyberPolicy(_)) | None => {}
        }
    }

    async fn apply_stream_policy_failure(
        &self,
        scope: &ControllerRequestScope,
        account_id: &str,
        classification: &StreamFailureClassification<'_>,
    ) {
        if matches!(
            classification.owner,
            Some(SharedFailureClassification::CyberPolicy(_))
        ) {
            self.cyber_policy
                .exclude_account(&scope.cyber_policy, account_id)
                .await;
        }
    }

    /// 按固定的反向 leave 顺序收敛 stream 终态；生命周期只调用这一 typed exit。
    pub(in crate::dispatch) async fn leave_stream(
        &self,
        scope: ControllerRequestScope,
        context: StreamControllerContext,
        summary: &crate::dispatch::lifecycle::stream::StreamSummary,
        body: &str,
    ) {
        let stream_failure = match &summary.terminal {
            StreamTerminal::Failed { failure } => {
                Some(self.classify_stream_failure(&context.account_id, failure))
            }
            _ => None,
        };
        let client_failure_status = stream_failure
            .as_ref()
            .map(|failure| i64::from(failure.client_failure().status_code()));
        let outcome = match &summary.terminal {
            StreamTerminal::Completed { .. } => FinalOutcome::Completed,
            StreamTerminal::Incomplete { .. } => FinalOutcome::Incomplete,
            StreamTerminal::Failed { .. }
            | StreamTerminal::UpstreamClosed
            | StreamTerminal::UpstreamError { .. }
            | StreamTerminal::ProtocolError { .. }
            | StreamTerminal::CaptureLimitExceeded => FinalOutcome::Failed,
            StreamTerminal::Cancelled | StreamTerminal::DownstreamClosed => FinalOutcome::Cancelled,
            StreamTerminal::Shutdown => FinalOutcome::Shutdown,
        };
        if !matches!(outcome, FinalOutcome::Cancelled | FinalOutcome::Shutdown)
            && let Some(classification) = stream_failure.as_ref()
        {
            self.apply_stream_account_state(classification).await;
        }
        let _ = best_effort_controller_io(
            "cloudflare",
            cloudflare::CloudflareController::leave_stream(cloudflare::StreamExit {
                recovery: &self.cloudflare,
                account_id: &context.account_id,
                set_cookie_headers: &context.set_cookie_headers,
                terminal: &summary.terminal,
            }),
        )
        .await;

        let fallback = usage::StreamUsageExit {
            rate_limit_headers: context.rate_limit_headers.clone(),
            turn_state: context.turn_state.clone(),
        };
        let mut usage_exit = best_effort_controller_io(
            "usage",
            usage::UsageController::leave_stream(usage::StreamExit {
                account_pool: &self.account_pool,
                account_id: &context.account_id,
                account_plan_type: context.account_plan_type.as_deref(),
                rate_limit_headers: &context.rate_limit_headers,
                rate_limit_header_updates: context.rate_limit_header_updates.as_ref(),
                turn_state_update: context.turn_state_update.as_ref(),
                turn_state: context.turn_state.as_deref(),
                usage: summary.usage,
            }),
        )
        .await
        .unwrap_or(fallback);

        let conversation_id = HistoryController::conversation_id(&scope.history, &context.request);
        let continuation_scope = HistoryController::continuation_scope(
            &context.request,
            context.transport,
            context.connection_local_continuation,
        );
        affinity::AffinityController::leave_stream(affinity::StreamExit {
            response: affinity::ResponseExit {
                affinity: &self.session_affinity,
                conversation_id,
                request: &context.request,
                account_id: &context.account_id,
                body,
                turn_state: usage_exit.turn_state.take(),
                usage: summary.usage,
                continuation_scope,
            },
            completed: matches!(summary.terminal, StreamTerminal::Completed { .. }),
        })
        .await;

        let _ = best_effort_controller_io(
            "telemetry",
            telemetry::TelemetryController::leave_stream(telemetry::StreamExit {
                context: telemetry::StreamContext {
                    recorder: &self.recorder,
                    account_id: &context.account_id,
                    request_id: &context.request_id,
                    route: &context.route,
                    display_model: &context.display_model,
                    requested_model: &context.requested_model,
                    request: &context.request,
                    transport: context.transport,
                    websocket_pool_decision: context.websocket_pool_decision,
                    diagnostics: &context.diagnostics,
                    response_metadata: &context.response_metadata,
                    transport_metrics: &context.transport_metrics,
                    attempt: &context.attempt,
                    attempts: &context.attempts,
                    started_at: context.started_at,
                },
                summary,
                rate_limit_headers: &usage_exit.rate_limit_headers,
                failure_status: client_failure_status,
                body,
            }),
        )
        .await;

        if !matches!(outcome, FinalOutcome::Cancelled | FinalOutcome::Shutdown) {
            if let Some(classification) = stream_failure.as_ref() {
                self.apply_stream_policy_failure(&scope, &context.account_id, classification)
                    .await;
            } else {
                self.finalize_stream_success(&scope, &summary.terminal)
                    .await;
            }
        }
    }

    /// 对一次 observation 只分类一次，并保证 feature effects 先于 decision 返回。
    pub(in crate::dispatch) async fn handle_attempt(
        &self,
        scope: &ControllerRequestScope,
        observation: &AttemptObservation,
    ) -> AttemptDecision {
        let classification = self.classify_attempt(observation);
        let _ = best_effort_controller_io(
            "usage.attempt",
            usage::UsageController::observe_attempt(&self.account_pool, &scope.usage, observation),
        )
        .await;

        if matches!(
            observation.kind,
            AttemptObservationKind::CompleteResponse(
                crate::dispatch::lifecycle::contract::CompleteResponseFacts::Completed
                    | crate::dispatch::lifecycle::contract::CompleteResponseFacts::Incomplete,
            )
        ) {
            self.cyber_policy.observe_success(&scope.cyber_policy).await;
        }

        match &classification {
            AttemptClassification::Shared(shared) => match shared {
                SharedFailureClassification::CyberPolicy(_) => {
                    if let Some(account_id) = observation
                        .account
                        .as_ref()
                        .map(|account| account.id.as_str())
                    {
                        self.cyber_policy
                            .exclude_account(&scope.cyber_policy, account_id)
                            .await;
                    }
                }
                SharedFailureClassification::AccountFailure(classified) => {
                    AccountFailureController::apply_effect(
                        &self.account_pool,
                        &self.codex,
                        classified,
                    )
                    .await;
                }
                SharedFailureClassification::Quota(classified) => {
                    QuotaController::apply_effect(&self.account_pool, &self.codex, classified)
                        .await;
                }
            },
            AttemptClassification::Cloudflare(failure) => {
                let _ = best_effort_controller_io(
                    "cloudflare.attempt",
                    cloudflare::CloudflareController::apply_effect(
                        &self.cloudflare,
                        &self.account_pool,
                        failure,
                    ),
                )
                .await;
            }
            AttemptClassification::History(_) | AttemptClassification::Default(_) => {}
        }

        self.attempt_decision(observation, classification)
    }

    fn classify_attempt<'a>(
        &self,
        observation: &'a AttemptObservation,
    ) -> AttemptClassification<'a> {
        let shared = Self::classify_shared_failure(FailureObservation::Attempt(observation));
        let shared = match shared {
            Some(shared @ SharedFailureClassification::CyberPolicy(_)) => {
                return AttemptClassification::Shared(shared);
            }
            shared => shared,
        };
        if let Some(classified) = cloudflare::CloudflareController::classify(observation) {
            return AttemptClassification::Cloudflare(classified);
        }
        if let Some(decision) = HistoryController::decide(observation) {
            return AttemptClassification::History(decision);
        }
        if let Some(shared) = shared {
            return AttemptClassification::Shared(shared);
        }
        AttemptClassification::Default(Self::default_attempt_decision(observation))
    }

    /// complete、prefetch 与 committed stream 共用这一份 feature owner 优先级。
    fn classify_shared_failure<'a>(
        observation: FailureObservation<'a>,
    ) -> Option<SharedFailureClassification<'a>> {
        let fact = observation.fact();
        if let Some(classified) = fact.and_then(CyberPolicyController::classify) {
            return Some(SharedFailureClassification::CyberPolicy(classified));
        }

        let account_failure = match observation {
            FailureObservation::Attempt(observation) => {
                AccountFailureController::classify(observation)
            }
            FailureObservation::Stream { account_id, .. } => {
                fact.and_then(|fact| AccountFailureController::classify_failure(account_id, fact))
            }
        };
        if let Some(classified) = account_failure {
            return Some(SharedFailureClassification::AccountFailure(classified));
        }

        let quota = match observation {
            FailureObservation::Attempt(observation) => QuotaController::classify(observation),
            FailureObservation::Stream { account_id, .. } => {
                fact.and_then(|fact| QuotaController::classify_failure(account_id, fact))
            }
        };
        quota.map(SharedFailureClassification::Quota)
    }

    fn attempt_decision(
        &self,
        observation: &AttemptObservation,
        classification: AttemptClassification<'_>,
    ) -> AttemptDecision {
        match classification {
            AttemptClassification::Shared(shared) => match shared {
                SharedFailureClassification::CyberPolicy(classified) => {
                    CyberPolicyController::decision(observation, classified)
                }
                SharedFailureClassification::AccountFailure(classified) => {
                    let client_failure = response_failure(observation).map(|failure| {
                        AccountFailureController::client_failure(&classified, failure)
                    });
                    with_response_client_failure(
                        AccountFailureController::decision(observation, classified),
                        client_failure,
                    )
                }
                SharedFailureClassification::Quota(classified) => {
                    let client_failure =
                        response_failure(observation).map(QuotaController::client_failure);
                    with_response_client_failure(
                        QuotaController::decision(observation, classified),
                        client_failure,
                    )
                }
            },
            AttemptClassification::Cloudflare(classified) => {
                cloudflare::CloudflareController::decision(observation, classified)
            }
            AttemptClassification::History(decision) => with_response_client_failure(
                decision,
                response_failure(observation)
                    .map(|failure| ClientFailure::new(failure.clone(), 400, false)),
            ),
            AttemptClassification::Default(decision) => decision,
        }
    }

    fn default_attempt_decision(observation: &AttemptObservation) -> AttemptDecision {
        match &observation.kind {
            AttemptObservationKind::CompleteResponse(
                crate::dispatch::lifecycle::contract::CompleteResponseFacts::Completed
                | crate::dispatch::lifecycle::contract::CompleteResponseFacts::Incomplete,
            ) => AttemptDecision::Accept,
            AttemptObservationKind::CompleteResponse(
                crate::dispatch::lifecycle::contract::CompleteResponseFacts::Failed(failure),
            ) => AttemptDecision::Return(AttemptReturnKind::Failed(
                default_response_client_failure(failure),
            )),
            AttemptObservationKind::CompleteResponse(
                crate::dispatch::lifecycle::contract::CompleteResponseFacts::MissingCompleted
                | crate::dispatch::lifecycle::contract::CompleteResponseFacts::Empty,
            ) => AttemptDecision::Return(AttemptReturnKind::Observed),
            AttemptObservationKind::StreamFailure(failure) => AttemptDecision::Return(
                AttemptReturnKind::Failed(default_response_client_failure(failure)),
            ),
            AttemptObservationKind::NoCandidate { .. } => {
                AttemptDecision::Return(AttemptReturnKind::Observed)
            }
            AttemptObservationKind::CandidatePreparationRejected => {
                unreachable!("candidate preparation controller owns it")
            }
            AttemptObservationKind::RoutePreparationRejected { message } => {
                AttemptDecision::Return(AttemptReturnKind::RouteUnavailable {
                    message: message.clone(),
                })
            }
            AttemptObservationKind::PinnedCandidateUnavailable { .. } => {
                unreachable!("route controller owns pinned-candidate acquisition failures")
            }
            AttemptObservationKind::ProtocolFailure { .. } => {
                AttemptDecision::Return(AttemptReturnKind::Observed)
            }
            AttemptObservationKind::UpstreamFailure(facts) => {
                if observation.routing.can_retry_next_candidate
                    && AccountFailureController::is_retryable_transport(observation)
                {
                    return AttemptDecision::RetryNextCandidate {
                        exhaustion: observation.account.as_ref().map(|account| {
                            crate::dispatch::failure::exhaustion::AccountExhaustionRecord::new(
                                account.id.clone(),
                                crate::dispatch::failure::exhaustion::ExhaustedAccountKind::UpstreamUnavailable,
                                facts.message.clone(),
                            )
                        }),
                        on_exhaustion: None,
                    };
                }
                AttemptDecision::Return(AttemptReturnKind::Observed)
            }
        }
    }
}

fn response_failure(observation: &AttemptObservation) -> Option<&ResponsesSseFailure> {
    match &observation.kind {
        AttemptObservationKind::CompleteResponse(
            crate::dispatch::lifecycle::contract::CompleteResponseFacts::Failed(failure),
        )
        | AttemptObservationKind::StreamFailure(failure) => Some(failure),
        _ => None,
    }
}

fn with_response_client_failure(
    decision: AttemptDecision,
    failure: Option<ClientFailure>,
) -> AttemptDecision {
    match (decision, failure) {
        (AttemptDecision::Return(AttemptReturnKind::Observed), Some(failure)) => {
            AttemptDecision::Return(AttemptReturnKind::Failed(failure))
        }
        (decision, _) => decision,
    }
}

fn default_response_client_failure(failure: &ResponsesSseFailure) -> ClientFailure {
    AccountFailureController::unowned_client_failure(failure)
        .unwrap_or_else(|| ClientFailure::new(failure.clone(), 502, false))
}

async fn best_effort_controller_io<T>(
    controller: &'static str,
    operation: impl Future<Output = T>,
) -> Option<T> {
    match timeout(BEST_EFFORT_IO_TIMEOUT, operation).await {
        Ok(output) => Some(output),
        Err(_) => {
            tracing::warn!(
                controller,
                timeout_ms = BEST_EFFORT_IO_TIMEOUT.as_millis(),
                "Timed out running best-effort controller I/O"
            );
            None
        }
    }
}
