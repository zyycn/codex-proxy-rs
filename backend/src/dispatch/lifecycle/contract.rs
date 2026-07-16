//! Attempt lifecycle 的类型化 observation、decision 与结果。

use bytes::Bytes;

use crate::{
    dispatch::{
        errors::{ClientFailure, ResponseDispatchError},
        failure::exhaustion::{AccountExhaustionRecord, ExhaustedAccount},
        transport::canonical::{CanonicalStreamBatch, CanonicalStreamDecoder},
    },
    fleet::{account::Account, pool::AccountLease},
    upstream::openai::{
        failure::UpstreamFailureFacts,
        protocol::{
            responses::{CodexResponsesRequest, CollectedResponse, ResponsesSseFailure},
            sse::SseError,
        },
        transport::{
            CodexBackendResponse, CodexBackendStreamingResponse, CodexBackendTransport,
            CodexClientError, CodexUpstreamDiagnostics,
        },
    },
};

use super::trace::{ResponseDispatchAttempt, ResponseDispatchTrace};

/// Request/Attempt/Stream 共同消费的最终结果类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::dispatch) enum FinalOutcome {
    Completed,
    Incomplete,
    Failed,
    Cancelled,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::dispatch) struct AttemptAccountFacts {
    pub id: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

impl From<&Account> for AttemptAccountFacts {
    fn from(account: &Account) -> Self {
        Self {
            id: account.id.clone(),
            email: account.email.clone(),
            plan_type: account.plan_type.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::dispatch) struct AttemptRoutingFacts {
    pub external_origin: bool,
    pub can_retry_same_account: bool,
    pub can_retry_next_candidate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::dispatch) struct CandidateLedgerFacts {
    pub candidates: usize,
    pub attempted: usize,
    pub state_excluded: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::dispatch) enum CompleteResponseFacts {
    Completed,
    Incomplete,
    Failed(ResponsesSseFailure),
    MissingCompleted,
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::dispatch) enum ProtocolFailureKind {
    InvalidSse,
    EmptyStream,
    NoCommitBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::dispatch) enum PinnedCandidateAcquireFailureKind {
    Busy,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::dispatch) enum AttemptObservationKind {
    NoCandidate {
        ledger: CandidateLedgerFacts,
        last_exhausted: Option<ExhaustedAccount>,
    },
    CandidatePreparationRejected,
    RoutePreparationRejected {
        message: String,
    },
    PinnedCandidateUnavailable {
        account_id: String,
        kind: PinnedCandidateAcquireFailureKind,
    },
    UpstreamFailure(UpstreamFailureFacts),
    ProtocolFailure {
        kind: ProtocolFailureKind,
        message: String,
    },
    CompleteResponse(CompleteResponseFacts),
    StreamFailure(ResponsesSseFailure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::dispatch) struct AttemptObservation {
    pub account: Option<AttemptAccountFacts>,
    pub attempt: Option<ResponseDispatchAttempt>,
    pub transport: CodexBackendTransport,
    pub routing: AttemptRoutingFacts,
    pub kind: AttemptObservationKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::dispatch) enum AttemptDecision {
    Accept,
    RetrySameAccount,
    RetryNextCandidate {
        exhaustion: Option<AccountExhaustionRecord>,
        on_exhaustion: Option<ClientFailure>,
    },
    Return(AttemptReturnKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::dispatch) enum AttemptReturnKind {
    Observed,
    Failed(ClientFailure),
    ContinuationBusy,
    RouteUnavailable { message: String },
}

pub(in crate::dispatch) struct EstablishedAttemptContext {
    pub request: CodexResponsesRequest,
    pub controller_scope: crate::dispatch::controllers::ControllerRequestScope,
    pub trace: ResponseDispatchTrace,
    pub account: Account,
    pub attempt: ResponseDispatchAttempt,
}

pub(in crate::dispatch) struct EstablishedComplete {
    pub context: EstablishedAttemptContext,
    pub response: CodexBackendResponse,
    pub body: EstablishedCompleteBody,
}

pub(in crate::dispatch) enum EstablishedCompleteBody {
    Completed(serde_json::Value),
    Incomplete(serde_json::Value),
}

impl TryFrom<CollectedResponse> for EstablishedCompleteBody {
    type Error = ();

    fn try_from(value: CollectedResponse) -> Result<Self, Self::Error> {
        match value {
            CollectedResponse::Completed(body) => Ok(Self::Completed(body)),
            CollectedResponse::Incomplete(body) => Ok(Self::Incomplete(body)),
            CollectedResponse::Failed(_)
            | CollectedResponse::MissingCompleted
            | CollectedResponse::Empty => Err(()),
        }
    }
}

pub(in crate::dispatch) struct EstablishedStream {
    pub context: EstablishedAttemptContext,
    pub attempt_request: CodexResponsesRequest,
    pub lease: AccountLease,
    pub response: CodexBackendStreamingResponse,
    pub decoder: CanonicalStreamDecoder,
    pub initial_batch: CanonicalStreamBatch,
    pub first_event_ms: i64,
}

pub(in crate::dispatch) enum EstablishedResponse {
    Complete(Box<EstablishedComplete>),
    Stream(Box<EstablishedStream>),
}

pub(in crate::dispatch) struct AttemptRejection {
    pub request: CodexResponsesRequest,
    pub trace: ResponseDispatchTrace,
    pub account_id: Option<String>,
    pub account: Option<Account>,
    pub attempt_request: Option<CodexResponsesRequest>,
    pub attempt: Option<ResponseDispatchAttempt>,
    pub transport: CodexBackendTransport,
    pub stream_failure: Option<RejectedStreamFailure>,
    pub error: ResponseDispatchError,
}

pub(in crate::dispatch) struct RejectedStreamFailure {
    pub failure: ResponsesSseFailure,
    pub prefetched: Bytes,
    pub diagnostics: CodexUpstreamDiagnostics,
    pub rate_limit_headers: Vec<(String, String)>,
}

pub(in crate::dispatch) enum AttemptApplyOutcome {
    Continue,
    Established(EstablishedResponse),
    Rejected(Box<AttemptRejection>),
}

#[derive(Debug, thiserror::Error)]
pub(in crate::dispatch) enum AttemptContractError {
    #[error("the previous attempt observation has not been decided")]
    DecisionRequired,
    #[error("attempt runner has already reached a terminal outcome")]
    Terminal,
    #[error("decision `{decision}` is invalid for observation `{observation}`")]
    InvalidDecision {
        decision: &'static str,
        observation: &'static str,
    },
}

pub(super) enum PendingProtocolFailure {
    InvalidSse(SseError),
    EmptyStream,
    NoCommitBoundary,
}

pub(super) struct PendingStreamResponse {
    pub account: Account,
    pub attempt_request: CodexResponsesRequest,
    pub attempt: ResponseDispatchAttempt,
    pub lease: AccountLease,
    pub response: CodexBackendStreamingResponse,
    pub prefetched: Bytes,
    pub decoder: CanonicalStreamDecoder,
    pub initial_batch: CanonicalStreamBatch,
    pub first_failure: Option<ResponsesSseFailure>,
    pub first_event_ms: i64,
}

pub(super) struct PendingCompleteResponse {
    pub account: Account,
    pub attempt_request: CodexResponsesRequest,
    pub attempt: ResponseDispatchAttempt,
    pub response: CodexBackendResponse,
    pub collected: CollectedResponse,
}

pub(super) enum PendingAttempt {
    NoCandidate,
    PinnedCandidateUnavailable {
        account_id: String,
        kind: PinnedCandidateAcquireFailureKind,
    },
    CandidatePreparationRejected {
        account: Account,
    },
    RoutePreparationRejected {
        account: Account,
        message: String,
    },
    UpstreamFailure {
        account: Account,
        attempt_request: CodexResponsesRequest,
        attempt: ResponseDispatchAttempt,
        transport: CodexBackendTransport,
        error: CodexClientError,
    },
    ProtocolFailure {
        account: Account,
        attempt_request: CodexResponsesRequest,
        attempt: ResponseDispatchAttempt,
        transport: CodexBackendTransport,
        error: PendingProtocolFailure,
    },
    CompleteResponse(Box<PendingCompleteResponse>),
    StreamResponse(Box<PendingStreamResponse>),
}
