//! 单账号 attempt 的 stepwise 执行器。

use std::time::Instant;

use serde_json::Value;

use crate::{
    dispatch::{
        affinity::AccountIdentityService,
        controllers::{
            AttemptAccountPreparation, AttemptAccountPreparationContext, AttemptRoutePreparation,
            AttemptRoutePreparationContext, AttemptUpstreamErrorContext, ControllerRequestScope,
            ControllerSet,
        },
        errors::{ClientFailure, ResponseDispatchError},
        failure::exhaustion::AccountExhaustionTracker,
        routing::candidates::AccountAttemptLedger,
        transport::{
            account::{
                AccountUpstreamContext, create_response_stream_with_account,
                create_response_with_account, prepare_response_transport_with_account,
            },
            canonical::{CanonicalStreamDecoder, normalize_complete_response},
            prefetch::{PrefetchedStream, StreamPrefetchError, prefetch_until_commit},
        },
    },
    fleet::{
        account::Account,
        pool::{AccountCandidateLease, AccountPoolService},
    },
    upstream::openai::{
        failure::upstream_failure_facts,
        protocol::responses::{
            CodexResponsesRequest, CollectedResponse, TransportRequirement, transport_requirement,
        },
        transport::{CodexBackendClient, CodexBackendStreamingResponse, CodexBackendTransport},
    },
};

use super::{
    contract::{
        AttemptAccountFacts, AttemptApplyOutcome, AttemptContractError, AttemptDecision,
        AttemptObservation, AttemptObservationKind, AttemptRejection, AttemptReturnKind,
        CandidateLedgerFacts, CompleteResponseFacts, EstablishedAttemptContext,
        EstablishedComplete, EstablishedCompleteBody, EstablishedResponse, EstablishedStream,
        PendingAttempt, PendingCompleteResponse, PendingProtocolFailure, PendingStreamResponse,
        PinnedCandidateAcquireFailureKind, ProtocolFailureKind,
    },
    trace::ResponseDispatchTrace,
};

pub(in crate::dispatch) enum AttemptMode {
    Complete { tuple_schema: Option<Value> },
    Stream { tuple_schema: Option<Value> },
}

pub(in crate::dispatch) struct AttemptRunnerDependencies<'a> {
    pub account_pool: &'a AccountPoolService,
    pub codex: &'a CodexBackendClient,
    pub controllers: &'a ControllerSet,
    pub account_identity: &'a AccountIdentityService,
    pub request_id: &'a str,
    pub route: &'a str,
    pub requested_model: &'a str,
    pub started_at: Instant,
}

/// 尚未越过提交边界的 attempt capability；只有该类型接受 retry decision。
pub(in crate::dispatch) struct OpenAttempt<'runner, 'dependencies> {
    runner: &'runner mut AttemptRunner<'dependencies>,
    observation: Box<AttemptObservation>,
}

impl OpenAttempt<'_, '_> {
    pub(in crate::dispatch) fn observation(&self) -> &AttemptObservation {
        &self.observation
    }

    pub(in crate::dispatch) fn controller_scope(
        &self,
    ) -> Result<&ControllerRequestScope, AttemptContractError> {
        self.runner.controller_scope()
    }

    pub(in crate::dispatch) async fn apply(
        self,
        decision: AttemptDecision,
    ) -> Result<AttemptApplyOutcome, AttemptContractError> {
        self.runner.apply_open(decision).await
    }
}

/// 已越过提交边界的 attempt capability；该类型只允许建立 stream。
pub(in crate::dispatch) struct CommittedAttempt {
    established: EstablishedResponse,
}

impl CommittedAttempt {
    pub(in crate::dispatch) fn accept(self) -> EstablishedResponse {
        self.established
    }
}

/// `next` 产出的提交 typestate；Committed 分支没有 decision/retry API。
pub(in crate::dispatch) enum AttemptStep<'runner, 'dependencies> {
    Open(OpenAttempt<'runner, 'dependencies>),
    Committed(CommittedAttempt),
}

pub(in crate::dispatch) struct AttemptRunner<'a> {
    dependencies: AttemptRunnerDependencies<'a>,
    mode: AttemptMode,
    request: Option<CodexResponsesRequest>,
    controller_scope: Option<ControllerRequestScope>,
    candidates: AccountAttemptLedger,
    exhaustion: AccountExhaustionTracker,
    on_exhaustion: Option<ClientFailure>,
    trace: Option<ResponseDispatchTrace>,
    required_account_id: Option<String>,
    pending: Option<PendingAttempt>,
}

impl<'a> AttemptRunner<'a> {
    pub(in crate::dispatch) fn new(
        dependencies: AttemptRunnerDependencies<'a>,
        mode: AttemptMode,
        request: CodexResponsesRequest,
        controller_scope: ControllerRequestScope,
        candidates: AccountAttemptLedger,
    ) -> Self {
        Self {
            dependencies,
            mode,
            request: Some(request),
            controller_scope: Some(controller_scope),
            candidates,
            exhaustion: AccountExhaustionTracker::default(),
            on_exhaustion: None,
            trace: Some(ResponseDispatchTrace::default()),
            required_account_id: None,
            pending: None,
        }
    }

    pub(in crate::dispatch) async fn next(
        &mut self,
    ) -> Result<AttemptStep<'_, 'a>, AttemptContractError> {
        if self.pending.is_some() {
            return Err(AttemptContractError::DecisionRequired);
        }

        let request = self
            .request
            .as_ref()
            .ok_or(AttemptContractError::Terminal)?;
        let acquired = if let Some(account_id) = self.required_account_id.take() {
            match self
                .dependencies
                .account_pool
                .acquire_candidate(request.model(), &account_id, chrono::Utc::now())
                .await
            {
                AccountCandidateLease::Acquired(acquired) => Some(*acquired),
                AccountCandidateLease::Busy => {
                    self.pending = Some(PendingAttempt::PinnedCandidateUnavailable {
                        account_id,
                        kind: PinnedCandidateAcquireFailureKind::Busy,
                    });
                    return self.step();
                }
                AccountCandidateLease::Unavailable => {
                    self.pending = Some(PendingAttempt::PinnedCandidateUnavailable {
                        account_id,
                        kind: PinnedCandidateAcquireFailureKind::Unavailable,
                    });
                    return self.step();
                }
            }
        } else {
            self.candidates
                .acquire_next(self.dependencies.account_pool)
                .await
        };
        let Some(acquired) = acquired else {
            self.pending = Some(PendingAttempt::NoCandidate);
            return self.step();
        };

        let account = acquired.account.clone();
        let controller_scope = self
            .controller_scope
            .as_mut()
            .ok_or(AttemptContractError::Terminal)?;
        let prepared_account = match self
            .dependencies
            .controllers
            .prepare_attempt_account(
                AttemptAccountPreparationContext {
                    codex: self.dependencies.codex,
                    account_identity: self.dependencies.account_identity,
                    request_id: self.dependencies.request_id,
                },
                acquired,
            )
            .await
        {
            AttemptAccountPreparation::Ready(prepared) => *prepared,
            AttemptAccountPreparation::Rejected => {
                self.pending = Some(PendingAttempt::CandidatePreparationRejected { account });
                return self.step();
            }
        };
        let acquired = prepared_account.lease;
        let cookie_header = prepared_account.cookie_header;

        let account_scoped_request = match self.dependencies.controllers.prepare_attempt_route(
            controller_scope,
            request,
            AttemptRoutePreparationContext {
                account_identity: self.dependencies.account_identity,
                account_id: &account.id,
            },
        ) {
            AttemptRoutePreparation::Ready(request) => *request,
            AttemptRoutePreparation::Unavailable { message } => {
                acquired.release_without_usage().await;
                self.pending = Some(PendingAttempt::RoutePreparationRejected { account, message });
                return self.step();
            }
        };

        let attempt = self
            .trace
            .as_mut()
            .ok_or(AttemptContractError::Terminal)?
            .start_attempt(&account.id);
        let upstream_context = AccountUpstreamContext {
            codex: self.dependencies.codex,
            request_id: self.dependencies.request_id,
            account: &account,
            cookie_header: cookie_header.as_deref(),
        };
        let ((), prepared_transport) = tokio::join!(
            self.dependencies
                .account_pool
                .wait_for_request_interval(&acquired),
            prepare_response_transport_with_account(upstream_context, &account_scoped_request),
        );
        let prepared_transport = match prepared_transport {
            Ok(prepared) => prepared,
            Err(error) => {
                let attempt_request = account_scoped_request.into_request();
                acquired.complete().await;
                let transport = error
                    .transport()
                    .unwrap_or_else(|| intended_transport(&attempt_request));
                observe_upstream_error(
                    self.dependencies.controllers,
                    AttemptUpstreamErrorContext {
                        request_id: self.dependencies.request_id,
                        account: &account,
                        route: self.dependencies.route,
                        model: self.dependencies.requested_model,
                        started_at: self.dependencies.started_at,
                        stream: matches!(self.mode, AttemptMode::Stream { .. }),
                        transport,
                        request: &attempt_request,
                        error: &error,
                        trace: self.trace.as_ref().ok_or(AttemptContractError::Terminal)?,
                        attempt: &attempt,
                    },
                )
                .await;
                self.pending = Some(PendingAttempt::UpstreamFailure {
                    account,
                    attempt_request,
                    attempt,
                    transport,
                    error,
                });
                return self.step();
            }
        };

        match &self.mode {
            AttemptMode::Complete { tuple_schema } => {
                let result = create_response_with_account(
                    upstream_context,
                    &account_scoped_request,
                    prepared_transport,
                    attempt.started_at(),
                )
                .await;
                let attempt_request = account_scoped_request.into_request();
                acquired.complete().await;
                self.pending = Some(match result {
                    Ok(response) => {
                        self.dependencies
                            .controllers
                            .observe_complete_upstream(
                                &account,
                                &response.set_cookie_headers,
                                &response.rate_limit_headers,
                            )
                            .await;
                        match normalize_complete_response(&response.body, tuple_schema.as_ref()) {
                            Ok(collected) => PendingAttempt::CompleteResponse(Box::new(
                                PendingCompleteResponse {
                                    account,
                                    attempt_request,
                                    attempt,
                                    response,
                                    collected,
                                },
                            )),
                            Err(error) => PendingAttempt::ProtocolFailure {
                                account,
                                attempt_request,
                                attempt,
                                transport: response.transport,
                                error: PendingProtocolFailure::InvalidSse(error),
                            },
                        }
                    }
                    Err(error) => {
                        let transport = error
                            .transport()
                            .unwrap_or_else(|| intended_transport(request));
                        observe_upstream_error(
                            self.dependencies.controllers,
                            AttemptUpstreamErrorContext {
                                request_id: self.dependencies.request_id,
                                account: &account,
                                route: self.dependencies.route,
                                model: self.dependencies.requested_model,
                                started_at: self.dependencies.started_at,
                                stream: matches!(self.mode, AttemptMode::Stream { .. }),
                                transport,
                                request: &attempt_request,
                                error: &error,
                                trace: self.trace.as_ref().ok_or(AttemptContractError::Terminal)?,
                                attempt: &attempt,
                            },
                        )
                        .await;
                        PendingAttempt::UpstreamFailure {
                            account,
                            attempt_request,
                            attempt,
                            transport,
                            error,
                        }
                    }
                });
            }
            AttemptMode::Stream { tuple_schema } => {
                let result = create_response_stream_with_account(
                    upstream_context,
                    &account_scoped_request,
                    prepared_transport,
                )
                .await;
                let attempt_request = account_scoped_request.into_request();
                self.pending = Some(match result {
                    Err(error) => {
                        acquired.complete().await;
                        let transport = error
                            .transport()
                            .unwrap_or_else(|| intended_transport(request));
                        observe_upstream_error(
                            self.dependencies.controllers,
                            AttemptUpstreamErrorContext {
                                request_id: self.dependencies.request_id,
                                account: &account,
                                route: self.dependencies.route,
                                model: self.dependencies.requested_model,
                                started_at: self.dependencies.started_at,
                                stream: matches!(self.mode, AttemptMode::Stream { .. }),
                                transport,
                                request: &attempt_request,
                                error: &error,
                                trace: self.trace.as_ref().ok_or(AttemptContractError::Terminal)?,
                                attempt: &attempt,
                            },
                        )
                        .await;
                        PendingAttempt::UpstreamFailure {
                            account,
                            attempt_request,
                            attempt,
                            transport,
                            error,
                        }
                    }
                    Ok(response) => {
                        let CodexBackendStreamingResponse {
                            body,
                            transport,
                            turn_state,
                            set_cookie_headers,
                            rate_limit_headers,
                            rate_limit_header_updates,
                            turn_state_update,
                            websocket_pool_decision,
                            diagnostics,
                            response_metadata,
                            transport_metrics,
                            connection_local_continuation,
                        } = response;
                        let mut decoder = CanonicalStreamDecoder::new(tuple_schema.clone());
                        match prefetch_until_commit(
                            body,
                            &mut decoder,
                            attempt_request.stream_commit_policy,
                            attempt.started_at(),
                        )
                        .await
                        {
                            Err(StreamPrefetchError::Upstream(error)) => {
                                acquired.complete().await;
                                observe_upstream_error(
                                    self.dependencies.controllers,
                                    AttemptUpstreamErrorContext {
                                        request_id: self.dependencies.request_id,
                                        account: &account,
                                        route: self.dependencies.route,
                                        model: self.dependencies.requested_model,
                                        started_at: self.dependencies.started_at,
                                        stream: matches!(self.mode, AttemptMode::Stream { .. }),
                                        transport,
                                        request: &attempt_request,
                                        error: &error,
                                        trace: self
                                            .trace
                                            .as_ref()
                                            .ok_or(AttemptContractError::Terminal)?,
                                        attempt: &attempt,
                                    },
                                )
                                .await;
                                PendingAttempt::UpstreamFailure {
                                    account,
                                    attempt_request,
                                    attempt,
                                    transport,
                                    error,
                                }
                            }
                            Err(StreamPrefetchError::Empty) => {
                                acquired.complete().await;
                                PendingAttempt::ProtocolFailure {
                                    account,
                                    attempt_request,
                                    attempt,
                                    transport,
                                    error: PendingProtocolFailure::EmptyStream,
                                }
                            }
                            Err(StreamPrefetchError::NoCommitBoundary) => {
                                acquired.complete().await;
                                PendingAttempt::ProtocolFailure {
                                    account,
                                    attempt_request,
                                    attempt,
                                    transport,
                                    error: PendingProtocolFailure::NoCommitBoundary,
                                }
                            }
                            Err(StreamPrefetchError::InvalidSse(error)) => {
                                acquired.complete().await;
                                PendingAttempt::ProtocolFailure {
                                    account,
                                    attempt_request,
                                    attempt,
                                    transport,
                                    error: PendingProtocolFailure::InvalidSse(error),
                                }
                            }
                            Ok(PrefetchedStream {
                                bytes: prefetched,
                                initial_batch,
                                body,
                                first_event_ms,
                            }) => {
                                let first_failure = initial_batch.first_failure();
                                PendingAttempt::StreamResponse(Box::new(PendingStreamResponse {
                                    account,
                                    attempt_request,
                                    attempt,
                                    lease: acquired,
                                    response: CodexBackendStreamingResponse {
                                        body,
                                        transport,
                                        turn_state,
                                        set_cookie_headers,
                                        rate_limit_headers,
                                        rate_limit_header_updates,
                                        turn_state_update,
                                        websocket_pool_decision,
                                        diagnostics,
                                        response_metadata,
                                        transport_metrics,
                                        connection_local_continuation,
                                    },
                                    prefetched,
                                    decoder,
                                    initial_batch,
                                    first_failure,
                                    first_event_ms,
                                }))
                            }
                        }
                    }
                });
            }
        }
        self.step()
    }

    pub(in crate::dispatch) fn controller_scope(
        &self,
    ) -> Result<&ControllerRequestScope, AttemptContractError> {
        self.controller_scope
            .as_ref()
            .ok_or(AttemptContractError::Terminal)
    }

    async fn apply_open(
        &mut self,
        decision: AttemptDecision,
    ) -> Result<AttemptApplyOutcome, AttemptContractError> {
        let pending = self
            .pending
            .take()
            .ok_or(AttemptContractError::DecisionRequired)?;
        match decision {
            AttemptDecision::Accept => self
                .establish(pending)
                .map(AttemptApplyOutcome::Established),
            AttemptDecision::RetrySameAccount => {
                let account_id = pending_account(&pending)
                    .ok_or_else(|| invalid("retry_same", &pending))?
                    .to_string();
                if !self.dependencies.controllers.prepare_same_account_retry(
                    self.controller_scope
                        .as_mut()
                        .ok_or(AttemptContractError::Terminal)?,
                    &account_id,
                ) {
                    return Err(invalid("retry_same", &pending));
                }
                release_pending_stream(pending).await;
                self.required_account_id = Some(account_id);
                Ok(AttemptApplyOutcome::Continue)
            }
            AttemptDecision::RetryNextCandidate {
                exhaustion,
                on_exhaustion,
            } => {
                release_pending_stream(pending).await;
                self.on_exhaustion = on_exhaustion;
                if let Some(exhaustion) = exhaustion {
                    self.exhaustion.record_exhaustion(exhaustion);
                }
                Ok(AttemptApplyOutcome::Continue)
            }
            AttemptDecision::Return(kind) => self.reject(pending, kind).await,
        }
    }

    fn step(&mut self) -> Result<AttemptStep<'_, 'a>, AttemptContractError> {
        let kind = {
            let pending = self
                .pending
                .as_ref()
                .ok_or(AttemptContractError::DecisionRequired)?;
            observation_kind(pending, &self.candidates, &self.exhaustion)
        };
        let Some(kind) = kind else {
            let pending = self
                .pending
                .take()
                .ok_or(AttemptContractError::DecisionRequired)?;
            return Ok(AttemptStep::Committed(CommittedAttempt {
                established: self.establish(pending)?,
            }));
        };
        let observation = self.observation(kind)?;
        Ok(AttemptStep::Open(OpenAttempt {
            runner: self,
            observation: Box::new(observation),
        }))
    }

    fn observation(
        &self,
        kind: AttemptObservationKind,
    ) -> Result<AttemptObservation, AttemptContractError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(AttemptContractError::DecisionRequired)?;
        let request = self
            .request
            .as_ref()
            .ok_or(AttemptContractError::Terminal)?;
        let controller_scope = self
            .controller_scope
            .as_ref()
            .ok_or(AttemptContractError::Terminal)?;
        let account = pending_account_value(pending);
        let account_id = account.map(|account| account.id.as_str());
        Ok(AttemptObservation {
            account: account.map(AttemptAccountFacts::from),
            attempt: pending_attempt(pending).cloned(),
            transport: pending_transport(pending).unwrap_or_else(|| intended_transport(request)),
            routing: self
                .dependencies
                .controllers
                .attempt_routing_facts(controller_scope, account_id),
            kind,
        })
    }

    fn establish(
        &mut self,
        pending: PendingAttempt,
    ) -> Result<EstablishedResponse, AttemptContractError> {
        let established = match pending {
            PendingAttempt::CompleteResponse(complete) => {
                let PendingCompleteResponse {
                    account,
                    attempt,
                    response,
                    collected,
                    ..
                } = *complete;
                let body = EstablishedCompleteBody::try_from(collected).map_err(|()| {
                    AttemptContractError::InvalidDecision {
                        decision: "establish",
                        observation: "non_successful_complete_response",
                    }
                })?;
                EstablishedResponse::Complete(Box::new(EstablishedComplete {
                    context: self.take_established_context(account, attempt)?,
                    response,
                    body,
                }))
            }
            PendingAttempt::StreamResponse(stream) => {
                let PendingStreamResponse {
                    account,
                    attempt,
                    lease,
                    response,
                    decoder,
                    initial_batch,
                    first_event_ms,
                    ..
                } = *stream;
                EstablishedResponse::Stream(Box::new(EstablishedStream {
                    context: self.take_established_context(account, attempt)?,
                    lease,
                    response,
                    decoder,
                    initial_batch,
                    first_event_ms,
                }))
            }
            other => return Err(invalid("establish", &other)),
        };
        Ok(established)
    }

    async fn reject(
        &mut self,
        pending: PendingAttempt,
        kind: AttemptReturnKind,
    ) -> Result<AttemptApplyOutcome, AttemptContractError> {
        let account = pending_account_value(&pending).cloned();
        let account_id = account
            .as_ref()
            .map(|account| account.id.clone())
            .or_else(|| self.exhaustion.last_account_id().map(ToString::to_string));
        let attempt_request = pending_attempt_request(&pending).cloned();
        let attempt = pending_attempt(&pending).cloned();
        let request = self
            .request
            .as_ref()
            .ok_or(AttemptContractError::Terminal)?;
        let transport = pending_transport(&pending).unwrap_or_else(|| intended_transport(request));
        let stream_failure = match &pending {
            PendingAttempt::StreamResponse(stream) => {
                stream.first_failure.as_ref().map(|failure| {
                    super::contract::RejectedStreamFailure {
                        failure: failure.clone(),
                        prefetched: stream.prefetched.clone(),
                        diagnostics: stream.response.diagnostics.clone(),
                        rate_limit_headers: stream.response.rate_limit_headers.clone(),
                    }
                })
            }
            _ => None,
        };
        let error =
            rejection_error(pending, kind, &self.exhaustion, self.on_exhaustion.take()).await?;
        let request = self.request.take().ok_or(AttemptContractError::Terminal)?;
        self.controller_scope
            .take()
            .ok_or(AttemptContractError::Terminal)?;
        let trace = self.trace.take().ok_or(AttemptContractError::Terminal)?;
        Ok(AttemptApplyOutcome::Rejected(Box::new(AttemptRejection {
            request,
            trace,
            account_id,
            account,
            attempt_request,
            attempt,
            transport,
            stream_failure,
            error,
        })))
    }

    fn take_established_context(
        &mut self,
        account: Account,
        attempt: super::trace::ResponseDispatchAttempt,
    ) -> Result<EstablishedAttemptContext, AttemptContractError> {
        Ok(EstablishedAttemptContext {
            request: self.request.take().ok_or(AttemptContractError::Terminal)?,
            controller_scope: self
                .controller_scope
                .take()
                .ok_or(AttemptContractError::Terminal)?,
            trace: self.trace.take().ok_or(AttemptContractError::Terminal)?,
            account,
            attempt,
        })
    }
}

async fn observe_upstream_error(
    controllers: &ControllerSet,
    context: AttemptUpstreamErrorContext<'_>,
) {
    controllers.observe_upstream_error(context).await;
}

fn observation_kind(
    pending: &PendingAttempt,
    candidates: &AccountAttemptLedger,
    exhaustion: &AccountExhaustionTracker,
) -> Option<AttemptObservationKind> {
    Some(match pending {
        PendingAttempt::NoCandidate => AttemptObservationKind::NoCandidate {
            ledger: CandidateLedgerFacts {
                candidates: candidates.candidate_count(),
                attempted: candidates.attempted_count(),
                state_excluded: candidates.state_excluded_count(),
            },
            last_exhausted: exhaustion.last_exhausted(),
        },
        PendingAttempt::PinnedCandidateUnavailable { account_id, kind } => {
            AttemptObservationKind::PinnedCandidateUnavailable {
                account_id: account_id.clone(),
                kind: *kind,
            }
        }
        PendingAttempt::CandidatePreparationRejected { .. } => {
            AttemptObservationKind::CandidatePreparationRejected
        }
        PendingAttempt::RoutePreparationRejected { message, .. } => {
            AttemptObservationKind::RoutePreparationRejected {
                message: message.clone(),
            }
        }
        PendingAttempt::UpstreamFailure { error, .. } => {
            AttemptObservationKind::UpstreamFailure(upstream_failure_facts(error))
        }
        PendingAttempt::ProtocolFailure { error, .. } => AttemptObservationKind::ProtocolFailure {
            kind: match error {
                PendingProtocolFailure::InvalidSse(_) => ProtocolFailureKind::InvalidSse,
                PendingProtocolFailure::EmptyStream => ProtocolFailureKind::EmptyStream,
                PendingProtocolFailure::NoCommitBoundary => ProtocolFailureKind::NoCommitBoundary,
            },
            message: match error {
                PendingProtocolFailure::InvalidSse(error) => error.to_string(),
                PendingProtocolFailure::EmptyStream => {
                    "upstream stream ended without data".to_string()
                }
                PendingProtocolFailure::NoCommitBoundary => {
                    "upstream stream ended before output or a terminal event".to_string()
                }
            },
        },
        PendingAttempt::CompleteResponse(complete) => {
            AttemptObservationKind::CompleteResponse(match &complete.collected {
                CollectedResponse::Completed(_) => CompleteResponseFacts::Completed,
                CollectedResponse::Incomplete(_) => CompleteResponseFacts::Incomplete,
                CollectedResponse::Failed(failure) => {
                    CompleteResponseFacts::Failed(failure.clone())
                }
                CollectedResponse::MissingCompleted => CompleteResponseFacts::MissingCompleted,
                CollectedResponse::Empty => CompleteResponseFacts::Empty,
            })
        }
        PendingAttempt::StreamResponse(stream) => {
            return stream
                .first_failure
                .clone()
                .map(AttemptObservationKind::StreamFailure);
        }
    })
}

async fn release_pending_stream(pending: PendingAttempt) {
    if let PendingAttempt::StreamResponse(stream) = pending {
        stream.lease.complete().await;
    }
}

async fn rejection_error(
    pending: PendingAttempt,
    kind: AttemptReturnKind,
    exhaustion: &AccountExhaustionTracker,
    on_exhaustion: Option<ClientFailure>,
) -> Result<ResponseDispatchError, AttemptContractError> {
    match kind {
        AttemptReturnKind::Failed(failure) => {
            release_pending_stream(pending).await;
            Ok(ResponseDispatchError::Failed(failure))
        }
        AttemptReturnKind::ContinuationBusy => Ok(ResponseDispatchError::ContinuationBusy),
        AttemptReturnKind::RouteUnavailable { message } => {
            Ok(ResponseDispatchError::HistoryUnavailable {
                upstream_error: message,
            })
        }
        AttemptReturnKind::Observed => match pending {
            PendingAttempt::NoCandidate | PendingAttempt::CandidatePreparationRejected { .. } => {
                Ok(on_exhaustion.map_or_else(
                    || {
                        exhaustion
                            .last_exhausted()
                            .map(ResponseDispatchError::from_exhausted_account)
                            .unwrap_or(ResponseDispatchError::NoActiveAccount)
                    },
                    ResponseDispatchError::Failed,
                ))
            }
            PendingAttempt::RoutePreparationRejected { message, .. } => {
                Ok(ResponseDispatchError::HistoryUnavailable {
                    upstream_error: message,
                })
            }
            PendingAttempt::PinnedCandidateUnavailable { .. } => {
                Err(invalid("return", &PendingAttempt::NoCandidate))
            }
            PendingAttempt::UpstreamFailure { error, .. } => {
                Ok(ResponseDispatchError::Upstream(error))
            }
            PendingAttempt::ProtocolFailure { error, .. } => Ok(match error {
                PendingProtocolFailure::InvalidSse(error) => {
                    ResponseDispatchError::InvalidSse(error)
                }
                PendingProtocolFailure::EmptyStream => ResponseDispatchError::EmptyUpstreamResponse,
                PendingProtocolFailure::NoCommitBoundary => ResponseDispatchError::MissingCompleted,
            }),
            PendingAttempt::CompleteResponse(complete) => match complete.collected {
                CollectedResponse::Failed(_) => Err(invalid(
                    "return_untyped_failure",
                    &PendingAttempt::NoCandidate,
                )),
                CollectedResponse::MissingCompleted => Ok(ResponseDispatchError::MissingCompleted),
                CollectedResponse::Empty => Ok(ResponseDispatchError::EmptyUpstreamResponse),
                CollectedResponse::Completed(_) | CollectedResponse::Incomplete(_) => {
                    Err(invalid("return", &PendingAttempt::NoCandidate))
                }
            },
            PendingAttempt::StreamResponse(stream) => match stream.first_failure {
                Some(failure) => {
                    stream.lease.complete().await;
                    let _ = failure;
                    Err(invalid(
                        "return_untyped_failure",
                        &PendingAttempt::NoCandidate,
                    ))
                }
                None => Err(invalid("return", &PendingAttempt::NoCandidate)),
            },
        },
    }
}

fn pending_account(pending: &PendingAttempt) -> Option<&str> {
    pending_account_value(pending).map(|account| account.id.as_str())
}

fn pending_account_value(pending: &PendingAttempt) -> Option<&Account> {
    match pending {
        PendingAttempt::NoCandidate | PendingAttempt::PinnedCandidateUnavailable { .. } => None,
        PendingAttempt::CandidatePreparationRejected { account }
        | PendingAttempt::RoutePreparationRejected { account, .. }
        | PendingAttempt::UpstreamFailure { account, .. }
        | PendingAttempt::ProtocolFailure { account, .. } => Some(account),
        PendingAttempt::CompleteResponse(complete) => Some(&complete.account),
        PendingAttempt::StreamResponse(stream) => Some(&stream.account),
    }
}

fn pending_attempt(pending: &PendingAttempt) -> Option<&super::trace::ResponseDispatchAttempt> {
    match pending {
        PendingAttempt::UpstreamFailure { attempt, .. }
        | PendingAttempt::ProtocolFailure { attempt, .. } => Some(attempt),
        PendingAttempt::CompleteResponse(complete) => Some(&complete.attempt),
        PendingAttempt::StreamResponse(stream) => Some(&stream.attempt),
        PendingAttempt::NoCandidate
        | PendingAttempt::PinnedCandidateUnavailable { .. }
        | PendingAttempt::CandidatePreparationRejected { .. }
        | PendingAttempt::RoutePreparationRejected { .. } => None,
    }
}

fn pending_attempt_request(pending: &PendingAttempt) -> Option<&CodexResponsesRequest> {
    match pending {
        PendingAttempt::UpstreamFailure {
            attempt_request, ..
        }
        | PendingAttempt::ProtocolFailure {
            attempt_request, ..
        } => Some(attempt_request),
        PendingAttempt::CompleteResponse(complete) => Some(&complete.attempt_request),
        PendingAttempt::StreamResponse(stream) => Some(&stream.attempt_request),
        PendingAttempt::NoCandidate
        | PendingAttempt::PinnedCandidateUnavailable { .. }
        | PendingAttempt::CandidatePreparationRejected { .. }
        | PendingAttempt::RoutePreparationRejected { .. } => None,
    }
}

fn pending_transport(
    pending: &PendingAttempt,
) -> Option<crate::upstream::openai::transport::CodexBackendTransport> {
    match pending {
        PendingAttempt::UpstreamFailure { transport, .. }
        | PendingAttempt::ProtocolFailure { transport, .. } => Some(*transport),
        PendingAttempt::CompleteResponse(complete) => Some(complete.response.transport),
        PendingAttempt::StreamResponse(stream) => Some(stream.response.transport),
        PendingAttempt::NoCandidate
        | PendingAttempt::PinnedCandidateUnavailable { .. }
        | PendingAttempt::CandidatePreparationRejected { .. }
        | PendingAttempt::RoutePreparationRejected { .. } => None,
    }
}

fn intended_transport(request: &CodexResponsesRequest) -> CodexBackendTransport {
    match transport_requirement(request) {
        TransportRequirement::HttpRequired => CodexBackendTransport::HttpSse,
        TransportRequirement::ExplicitWebSocketWarmup
        | TransportRequirement::ExactWebSocketContinuation
        | TransportRequirement::PersistedContinuation
        | TransportRequirement::ExternalUnknown
        | TransportRequirement::NewChain => CodexBackendTransport::WebSocket,
    }
}

fn invalid(decision: &'static str, pending: &PendingAttempt) -> AttemptContractError {
    AttemptContractError::InvalidDecision {
        decision,
        observation: match pending {
            PendingAttempt::NoCandidate => "no_candidate",
            PendingAttempt::PinnedCandidateUnavailable { .. } => "pinned_candidate_unavailable",
            PendingAttempt::CandidatePreparationRejected { .. } => "candidate_preparation_rejected",
            PendingAttempt::RoutePreparationRejected { .. } => "route_preparation_rejected",
            PendingAttempt::UpstreamFailure { .. } => "upstream_failure",
            PendingAttempt::ProtocolFailure { .. } => "protocol_failure",
            PendingAttempt::CompleteResponse(_) => "complete_response",
            PendingAttempt::StreamResponse(_) => "stream_response",
        },
    }
}
