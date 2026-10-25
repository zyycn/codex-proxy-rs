//! 校验插件命令行注册声明并准备可执行命令

use super::PluginCommand;
use crate::{RpcSession, callback::PluginCallbacks};
use gateway_admin::model::{AdminError, plugins::instances::PluginInstance};
use gateway_plugin_sdk::{Capability, Manifest, Stage, call::management::CommandRegistration};
use std::sync::Arc;

const MAXIMUM_REGISTRATION_BYTES: usize = 64 * 1024;

pub(crate) async fn prepare(
    instance: &PluginInstance,
    manifest: &Manifest,
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
) -> Result<Vec<Arc<PluginCommand>>, AdminError> {
    let Some(declaration) = manifest.contributes.get(&Capability::CommandLine) else {
        return Ok(Vec::new());
    };
    if declaration.stages != [Stage::CommandLine] {
        return Err(AdminError::invalid("命令行能力必须声明 command_line 阶段"));
    }
    let reply = session
        .call(
            "command_line.register",
            session.context(Stage::Registration, session.budget(Stage::Registration)),
            serde_json::json!({}),
            Vec::new(),
        )
        .await
        .map_err(|_| AdminError::invalid("插件命令注册失败"))?;
    if reply.payload.len() > MAXIMUM_REGISTRATION_BYTES {
        return Err(AdminError::invalid("插件命令注册信封无效或过大"));
    }
    let registration: CommandRegistration = serde_json::from_slice(&reply.payload)
        .map_err(|_| AdminError::invalid("插件命令注册数据无效"))?;
    if registration.commands.len() > 32 {
        return Err(AdminError::invalid("每个插件最多注册 32 个命令"));
    }
    registration
        .commands
        .into_iter()
        .map(|descriptor| {
            if descriptor.parameters.len() > 64 {
                return Err(AdminError::invalid("插件命令参数超过容量限制"));
            }
            Ok(Arc::new(PluginCommand {
                instance_id: instance.id.clone(),
                artifact_sha256: instance.artifact_sha256.clone(),
                revision: instance.revision,
                descriptor,
                session: session.clone(),
                callbacks: callbacks.clone(),
            }))
        })
        .collect()
}
