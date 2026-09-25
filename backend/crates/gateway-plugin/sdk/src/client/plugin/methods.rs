//! 类型化业务方法目录；控制参数、敏感载荷、阶段与响应流在此固定。

use crate::{
    Capability as C, Stage as S,
    call::{catalog, frontend_authentication as frontend, host, management, observation, policy},
};

use super::{
    Empty, Method,
    typed::{
        decode_metadata, decode_metadata_without_payload, decode_payload, encode_metadata,
        encode_metadata_with_payload, encode_payload,
    },
};

pub const MODEL_CATALOG_REGISTER: Method<Empty, catalog::ModelCatalogRegistration> = Method::new(
    "model_catalog.register",
    &[C::ModelCatalog],
    &[S::Registration],
    decode_metadata_without_payload,
    encode_payload,
);

pub const RETRY_DECISION: Method<policy::RetryDecisionRequest, policy::RetryDecision> = Method::new(
    "policy.retry_decision",
    &[C::RetryPolicy],
    &[S::Retry],
    decode_metadata_without_payload,
    encode_metadata,
);

pub const MANAGEMENT_REGISTER: Method<Empty, management::ManagementRegistration> = Method::new(
    "management.register",
    &[C::Management],
    &[S::Registration],
    decode_metadata_without_payload,
    encode_payload,
);
pub const MANAGEMENT_HANDLE: Method<management::ManagementRequest, management::ManagementResponse> =
    Method::new(
        "management.handle",
        &[C::Management],
        &[S::Management],
        decode_metadata,
        encode_metadata_with_payload,
    );
pub const MANAGEMENT_CALLBACK: Method<
    management::ManagementRequest,
    management::ManagementResponse,
> = Method::new(
    "management.callback",
    &[C::Management],
    &[S::PublicManagement],
    decode_metadata_without_payload,
    encode_metadata_with_payload,
);
pub const COMMAND_LINE_REGISTER: Method<Empty, management::CommandRegistration> = Method::new(
    "command_line.register",
    &[C::CommandLine],
    &[S::Registration],
    decode_metadata_without_payload,
    encode_payload,
);
pub const COMMAND_LINE_EXECUTE: Method<management::CommandInvocation, management::CommandResult> =
    Method::new(
        "command_line.execute",
        &[C::CommandLine],
        &[S::CommandLine],
        decode_payload,
        encode_payload,
    );
pub const ROUTE_MODEL: Method<policy::ModelRouteRequest, policy::ModelRouteDecision> = Method::new(
    "policy.route_model",
    &[C::ModelRouter],
    &[S::Routing],
    decode_metadata,
    encode_metadata,
);
pub const SCHEDULE_ACCOUNT: Method<
    policy::AccountScheduleRequest,
    policy::AccountScheduleDecision,
> = Method::new(
    "policy.schedule_account",
    &[C::Scheduler],
    &[S::Scheduling],
    decode_metadata_without_payload,
    encode_metadata,
);
pub const OBSERVE_REQUEST: Method<policy::ObserveRequest, Empty> = Method::new(
    "policy.observe_request",
    &[C::RequestLifecycle, C::Usage],
    &[S::Observation],
    decode_payload,
    encode_metadata,
);
pub const OBSERVE_WEBSOCKET: Method<observation::ObserveWebSocketResponse, Empty> = Method::new(
    "websocket.response_event",
    &[C::WebSocketObserver],
    &[S::Observation],
    decode_metadata,
    encode_metadata,
);
pub const FRONTEND_IDENTIFIER: Method<Empty, frontend::FrontendAuthenticationIdentifier> =
    Method::new(
        "frontend_auth.identifier",
        &[C::FrontendAuthentication],
        &[S::Registration],
        decode_metadata_without_payload,
        encode_metadata,
    );
pub const FRONTEND_AUTHENTICATE: Method<
    frontend::FrontendAuthenticationRequest,
    frontend::FrontendAuthenticationResult,
> = Method::new(
    "frontend_auth.authenticate",
    &[C::FrontendAuthentication],
    &[S::Authentication],
    decode_payload,
    encode_payload,
);
pub const STATE_MIGRATE: Method<host::StateMigrationRequest, host::StateMigrationResult> =
    Method::new(
        "plugin.state.migrate",
        &[],
        &[S::Configuration],
        decode_payload,
        encode_payload,
    );
