//! Responses 请求进入 dispatch 的共享生命周期阶段。

use chrono::Utc;
use serde_json::Value;

use crate::{
    dispatch::{
        affinity::{AccountIdentityService, prepare_variant_identity},
        controllers::{ControllerEnter, ControllerRequestScope, ControllerSet},
        routing::candidates::AccountAttemptLedger,
    },
    fleet::pool::{AccountAcquireRequest, AccountPoolService},
    models::service::ModelService,
    upstream::openai::protocol::responses::CodexResponsesRequest,
};

/// Responses 请求的执行模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dispatch) enum RequestMode {
    Complete,
    Stream,
}

impl RequestMode {
    fn stream(self) -> bool {
        matches!(self, Self::Stream)
    }
}

/// 请求 enter 阶段所需的窄依赖集合。
pub(in crate::dispatch) struct RequestEnterDependencies<'a> {
    pub account_pool: &'a AccountPoolService,
    pub models: &'a ModelService,
    pub account_identity: &'a AccountIdentityService,
    pub controllers: ControllerSet,
}

/// 已完成请求级 enter 的 Responses 上下文。
///
/// 该类型只承载请求、会话恢复、会话策略和候选快照；账号 attempt
/// 与流生命周期由后续 pipeline 接管。
pub(in crate::dispatch) struct RequestContext {
    pub request: CodexResponsesRequest,
    pub display_model: String,
    pub compact: bool,
    pub tuple_schema: Option<Value>,
    pub image_generation_requested: bool,
    pub controllers: ControllerSet,
    pub controller_scope: ControllerRequestScope,
    pub candidates: AccountAttemptLedger,
}

/// 执行所有 Responses 模式共享的请求 enter 阶段。
pub(in crate::dispatch) async fn enter_request(
    dependencies: RequestEnterDependencies<'_>,
    mut request: CodexResponsesRequest,
    requested_model: &str,
    mode: RequestMode,
) -> RequestContext {
    let catalog = dependencies.models.catalog().await;
    let display_model = catalog.resolve_model_id(requested_model);
    request.set_model(display_model.clone());
    debug_assert_eq!(request.stream(), mode.stream());

    let compact = request.semantics().compact;
    let tuple_schema = request.tuple_schema.clone();
    let image_generation_requested = request.expects_image_generation();
    let now = Utc::now();
    prepare_variant_identity(&mut request);
    dependencies
        .account_identity
        .prepare_local_identity(&mut request);
    let controllers = dependencies.controllers;
    let ControllerEnter {
        scope,
        preferred_account_id,
        excluded_account_ids,
    } = controllers.enter(&request, now).await;

    let mut acquire_request = AccountAcquireRequest::new(request.model(), now);
    if let Some(preferred_account_id) = preferred_account_id {
        acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
    }
    let candidates = AccountAttemptLedger::freeze(
        dependencies.account_pool,
        &acquire_request,
        &excluded_account_ids,
    )
    .await;

    RequestContext {
        request,
        display_model,
        compact,
        tuple_schema,
        image_generation_requested,
        controllers,
        controller_scope: scope,
        candidates,
    }
}
