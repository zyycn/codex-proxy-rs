//! Attempt 生命周期的唯一静态 pipeline。

use crate::{
    dispatch::{controllers::ControllerRequestScope, routing::candidates::AccountAttemptLedger},
    upstream::openai::protocol::responses::CodexResponsesRequest,
};

use super::{
    attempt::{AttemptMode, AttemptRunner, AttemptRunnerDependencies, AttemptStep},
    contract::{AttemptApplyOutcome, AttemptContractError, AttemptRejection, EstablishedResponse},
};

pub(in crate::dispatch) enum AttemptPipelineOutcome {
    Established(EstablishedResponse),
    Rejected(Box<AttemptRejection>),
}

/// 执行完整的 attempt/retry/commit 状态机。
///
/// mode 适配器只能消费最终结果，不能自行复制 retry loop 或越过 commit 边界。
pub(in crate::dispatch) async fn run_attempt_pipeline(
    dependencies: AttemptRunnerDependencies<'_>,
    mode: AttemptMode,
    request: CodexResponsesRequest,
    controller_scope: ControllerRequestScope,
    candidates: AccountAttemptLedger,
) -> Result<AttemptPipelineOutcome, AttemptContractError> {
    let controllers = dependencies.controllers;
    let mut runner = AttemptRunner::new(dependencies, mode, request, controller_scope, candidates);

    loop {
        let outcome = match runner.next().await? {
            AttemptStep::Open(attempt) => {
                let decision = controllers
                    .handle_attempt(attempt.controller_scope()?, attempt.observation())
                    .await;
                attempt.apply(decision).await?
            }
            AttemptStep::Committed(attempt) => {
                return Ok(AttemptPipelineOutcome::Established(attempt.accept()));
            }
        };
        match outcome {
            AttemptApplyOutcome::Continue => {}
            AttemptApplyOutcome::Established(established) => {
                return Ok(AttemptPipelineOutcome::Established(established));
            }
            AttemptApplyOutcome::Rejected(rejection) => {
                return Ok(AttemptPipelineOutcome::Rejected(rejection));
            }
        }
    }
}
