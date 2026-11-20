//! Key 绑定分组的功能权限在请求入口与上游发送前生效

use crate::{
    account::scope::FrozenAccountScope, engine::middleware::MiddlewareHeader,
    error::DeniedGroupFeature, operation::Operation,
};

pub(super) fn check_group_permissions(
    scope: &FrozenAccountScope,
    operation: &Operation,
    headers: &[MiddlewareHeader],
) -> Result<(), DeniedGroupFeature> {
    if !scope.allows_image_generation() && operation.image_generation_requested() {
        return Err(DeniedGroupFeature::ImageGeneration);
    }
    if !scope.allows_subagents()
        && let Operation::Generate(request) = operation
        && request.protocol_payload().protocol() == gateway_protocol::openai::OPENAI_PROTOCOL
    {
        let payload = request.protocol_payload();
        let header = |name: &str| {
            headers
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case(name))
                .and_then(|header| std::str::from_utf8(header.value()).ok())
        };
        if gateway_protocol::openai::codex::collaboration_subagent_requested(
            payload.body(),
            payload.context(),
            header,
        ) {
            return Err(DeniedGroupFeature::CollaborativeSubagents);
        }
    }
    Ok(())
}
