//! Admin 端口回调共享的审计上下文、结果编码与错误映射

use gateway_admin::model::{AdminError, AdminErrorKind, MutationActor, MutationContext};
use gateway_plugin_sdk::{CallContext, ErrorCode, PluginFault};

use crate::RpcReply;

pub(super) fn mutation_context(context: &CallContext) -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        // 回调沿用宿主签发的调用上下文，插件不能自报审计身份
        request_id: format!(
            "plugin:{}:scope:{}:call:{}",
            context.instance_id, context.resource_scope_id, context.call_id
        ),
    }
}

pub(super) fn encode(value: &impl serde::Serialize) -> Result<RpcReply, PluginFault> {
    Ok(RpcReply {
        result: serde_json::json!({}),
        payload: serde_json::to_vec(value).map_err(|_| super::invalid())?,
    })
}

pub(super) fn map_admin_error(error: AdminError) -> PluginFault {
    let code = match error.kind() {
        AdminErrorKind::Invalid => ErrorCode::InvalidInput,
        AdminErrorKind::Unauthorized | AdminErrorKind::Forbidden => ErrorCode::PermissionDenied,
        AdminErrorKind::NotFound => ErrorCode::Rejected,
        AdminErrorKind::Conflict => ErrorCode::Conflict,
        AdminErrorKind::RateLimited => ErrorCode::Capacity,
        AdminErrorKind::UpstreamResultUnknown => ErrorCode::Uncertain,
        AdminErrorKind::BadGateway => ErrorCode::Upstream,
        AdminErrorKind::Unavailable | AdminErrorKind::Internal => ErrorCode::Fault,
    };
    PluginFault::new(code, error.message())
}
