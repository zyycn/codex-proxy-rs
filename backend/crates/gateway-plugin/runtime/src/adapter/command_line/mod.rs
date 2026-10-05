//! 插件命令行能力的会话、命令查找与帮助入口

mod duration;
mod execution;
mod parameters;
mod registration;

pub(crate) use registration::prepare;

use std::sync::Arc;

use gateway_admin::{
    model::{AdminError, Revision},
    ports::plugins::PluginStore,
};
use gateway_core::runtime::extensions::ExtensionSetReference;
use gateway_plugin_sdk::call::management::CommandDescriptor;

use crate::{RpcLimits, RpcSession, callback::PluginCallbacks};

pub(crate) struct PluginCommand {
    instance_id: String,
    artifact_sha256: String,
    revision: Revision,
    descriptor: CommandDescriptor,
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
}

/// 命令列表与执行共同保活已准备集合；这里不维护另一份当前代次
pub struct PluginCommandSession {
    _reference: ExtensionSetReference,
    commands: Vec<Arc<PluginCommand>>,
    sessions: Vec<Arc<RpcSession>>,
    store: Arc<dyn PluginStore>,
    limits: RpcLimits,
}

/// 输出只交付显式 CLI 调用方，不作为诊断内容
pub struct PluginCommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
    pub saved_accounts: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginCommandError {
    #[error(transparent)]
    Invalid(#[from] AdminError),
    #[error(
        "插件命令未完整完成（{reason}），已确认保存 {saved_accounts} 个账号，其余副作用结果未知；请核对后再决定是否重试"
    )]
    Incomplete {
        saved_accounts: usize,
        reason: &'static str,
    },
}

impl PluginCommandSession {
    pub(crate) fn new(
        reference: ExtensionSetReference,
        commands: Vec<Arc<PluginCommand>>,
        sessions: Vec<Arc<RpcSession>>,
        store: Arc<dyn PluginStore>,
        limits: RpcLimits,
    ) -> Self {
        Self {
            _reference: reference,
            commands,
            sessions,
            store,
            limits,
        }
    }

    /// CLI 专属终止入口；调用方不能将该集合同时发布给运行中的 HTTP 网关
    /// 包括未声明命令、但为候选校验而启动的进程，全部回收后才允许退出主 Runtime
    pub async fn shutdown(self) {
        for session in &self.sessions {
            session.quiesce();
        }
        futures::future::join_all(
            self.sessions
                .iter()
                .map(|session| session.shutdown(std::time::Duration::from_secs(1))),
        )
        .await;
    }

    pub fn help(
        &self,
        instance_id: Option<&str>,
        name: Option<&str>,
    ) -> Result<String, AdminError> {
        if let (Some(instance_id), Some(name)) = (instance_id, name) {
            let command = self.find(instance_id, name)?;
            return Ok(format!(
                "用法: codex-proxy-rs plugin {instance_id} {name} [参数]\n{}",
                parameters::help(&command.descriptor)
            ));
        }
        let mut output = String::from("用法: codex-proxy-rs plugin <实例 ID> <命令> [参数]\n\n");
        let mut found = false;
        for command in &self.commands {
            if instance_id.is_none_or(|id| id == command.instance_id) {
                found = true;
                output.push_str(&format!(
                    "  {} {}  {}\n",
                    command.instance_id, command.descriptor.name, command.descriptor.description
                ));
            }
        }
        if !found && instance_id.is_some() {
            return Err(AdminError::not_found("插件实例没有可用命令"));
        }
        if !found {
            output.push_str("没有已启用的插件命令\n");
        }
        Ok(output)
    }

    fn find(&self, instance_id: &str, name: &str) -> Result<&PluginCommand, AdminError> {
        self.commands
            .iter()
            .find(|command| command.instance_id == instance_id && command.descriptor.name == name)
            .map(AsRef::as_ref)
            .ok_or_else(|| AdminError::not_found("插件命令不存在或未启用"))
    }
}
