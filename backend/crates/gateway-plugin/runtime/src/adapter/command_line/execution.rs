//! 执行插件命令行调用，传递取消信号并映射 RPC 结果

use gateway_admin::model::AdminError;
use gateway_core::lifecycle::CancellationToken;
use gateway_plugin_sdk::{Stage, call::management::CommandResult};

use super::{
    PluginCommand, PluginCommandError, PluginCommandOutput, PluginCommandSession, parameters,
};
use crate::{RpcError, RpcLimits};

const MAXIMUM_OUTPUT_BYTES: usize = 64 * 1024;

impl PluginCommandSession {
    pub async fn execute(
        &self,
        instance_id: &str,
        name: &str,
        arguments: &[String],
    ) -> Result<PluginCommandOutput, PluginCommandError> {
        self.execute_cancellable(instance_id, name, arguments, &CancellationToken::new())
            .await
    }

    pub async fn execute_cancellable(
        &self,
        instance_id: &str,
        name: &str,
        arguments: &[String],
        cancellation: &CancellationToken,
    ) -> Result<PluginCommandOutput, PluginCommandError> {
        let command = self.find(instance_id, name)?;
        let invocation = parameters::parse(&command.descriptor, arguments)?;
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|_| AdminError::unavailable("无法复核插件命令授权"))?;
        if !snapshot.instances.iter().any(|instance| {
            instance.id == command.instance_id
                && instance.enabled
                && instance.trusted_process
                && instance.revision == command.revision
                && instance.artifact_sha256 == command.artifact_sha256
        }) {
            return Err(AdminError::conflict("插件命令配置或授权已变化，请重新读取命令").into());
        }
        let payload = serde_json::to_vec(&invocation)
            .map_err(|_| AdminError::invalid("插件命令参数无法编码"))?;
        let mut saved_accounts = 0;
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Ok(Err("调用已取消")),
            result = tokio::time::timeout(
                self.limits.maximum_call_timeout,
                command.execute(payload, self.limits, &mut saved_accounts),
            ) => result,
        };
        match result {
            Ok(Ok(output)) => Ok(output),
            // 一旦执行已开始，账号/私有状态回调也可能提交；仅 HTTP 未发送不能证明无副作用
            Ok(Err(reason)) => Err(PluginCommandError::Incomplete {
                saved_accounts,
                reason,
            }),
            Err(_) => Err(PluginCommandError::Incomplete {
                saved_accounts,
                reason: "超过命令总期限",
            }),
        }
    }
}

impl PluginCommand {
    async fn execute(
        &self,
        payload: Vec<u8>,
        limits: RpcLimits,
        saved_accounts: &mut usize,
    ) -> Result<PluginCommandOutput, &'static str> {
        let mut context = self
            .session
            .context(Stage::CommandLine, limits.maximum_call_timeout);
        context.request_id = Some(uuid::Uuid::new_v4().to_string());
        let scope = self
            .callbacks
            .prepare_command_line(&context)
            .map_err(|_| "回调授权上下文无效")?;
        let reply = self
            .session
            .call(
                "command_line.execute",
                context.clone(),
                serde_json::json!({}),
                payload,
            )
            .await
            .map_err(rpc_failure)?;
        if reply.result != serde_json::json!({})
            || reply.payload.len() > limits.maximum_buffered_body_bytes
        {
            self.session.invalid_response(Stage::CommandLine);
            return Err("结果信封无效");
        }
        let result: CommandResult = serde_json::from_slice(&reply.payload).map_err(|_| {
            self.session.invalid_response(Stage::CommandLine);
            "结果数据无效"
        })?;
        if result.stdout.len() > MAXIMUM_OUTPUT_BYTES
            || result.stderr.len() > MAXIMUM_OUTPUT_BYTES
            || result.accounts.len() > 32
        {
            self.session.invalid_response(Stage::CommandLine);
            return Err("结果大小或账号范围无效");
        }
        if result.exit_code == 0 {
            for account in result.accounts {
                self.callbacks
                    .save_command_account(&context, &scope, account)
                    .await
                    .map_err(|_| "账号提交未确认")?;
                *saved_accounts += 1;
            }
        }
        Ok(PluginCommandOutput {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code,
            saved_accounts: *saved_accounts,
        })
    }
}

fn rpc_failure(error: RpcError) -> &'static str {
    match error {
        RpcError::Timeout => "插件响应超时",
        RpcError::Cancelled => "调用已取消",
        RpcError::Closed | RpcError::Start(_) => "插件进程不可用",
        RpcError::Handshake | RpcError::Protocol | RpcError::InvalidResponse(_) => "插件协议错误",
        RpcError::Capacity => "插件调用容量不足",
        RpcError::Context => "调用上下文无效",
        RpcError::Remote(_) => "插件拒绝命令",
    }
}
