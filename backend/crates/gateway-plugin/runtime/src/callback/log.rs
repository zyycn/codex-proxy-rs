//! 插件日志回调的输入校验、输出限额与宿主日志写入

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use gateway_core::diagnostics::{body_fingerprint, diagnostic_json};
use gateway_plugin_sdk::{
    CallContext, Manifest, PluginFault,
    call::host::{LogLevel, LogRequest, LogResult},
};
use tokio::{sync::Semaphore, time::Instant};

use crate::RpcReply;

// 这是保护宿主的有界默认值，不是吞吐保证；预算按实例 incarnation 隔离
const WINDOW: Duration = Duration::from_secs(1);
const MAXIMUM_EVENTS: u32 = 64;
const MAXIMUM_INPUT_BYTES: usize = 8 * 1024;
const MAXIMUM_FIELDS: usize = 32;

pub(super) struct PluginLog {
    plugin_id: String,
    plugin_version: String,
    budget: Mutex<Budget>,
    slots: Arc<Semaphore>,
}

struct Budget {
    started: Instant,
    recorded: u32,
    suppressed: u64,
}

impl PluginLog {
    pub(super) fn new(
        manifest: &Manifest,
        slots: Arc<Semaphore>,
    ) -> Result<Self, gateway_plugin_sdk::ManifestError> {
        Ok(Self {
            plugin_id: manifest.plugin_id()?,
            plugin_version: manifest.version.to_string(),
            slots,
            budget: Mutex::new(Budget {
                started: Instant::now(),
                recorded: 0,
                suppressed: 0,
            }),
        })
    }

    pub(super) fn record(
        &self,
        context: &CallContext,
        params: serde_json::Value,
        payload: &[u8],
    ) -> Result<RpcReply, PluginFault> {
        if !payload.is_empty()
            || serde_json::to_vec(&params)
                .map_err(|_| super::invalid())?
                .len()
                > MAXIMUM_INPUT_BYTES
        {
            return Err(super::invalid());
        }
        let request: LogRequest = serde_json::from_value(params).map_err(|_| super::invalid())?;
        if request.event.is_empty()
            || request.event.len() > 64
            || !request
                .event
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || request.fields.len() > MAXIMUM_FIELDS
        {
            return Err(super::invalid());
        }
        let (suppressed, slot) = {
            let mut budget = self
                .budget
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if budget.started.elapsed() >= WINDOW {
                budget.started = Instant::now();
                budget.recorded = 0;
            }
            if budget.recorded == MAXIMUM_EVENTS {
                budget.suppressed = budget.suppressed.saturating_add(1);
                return reply(false);
            }
            let Ok(slot) = self.slots.clone().try_acquire_owned() else {
                budget.suppressed = budget.suppressed.saturating_add(1);
                return reply(false);
            };
            budget.recorded += 1;
            (std::mem::take(&mut budget.suppressed), slot)
        };
        // 使用现有诊断 owner；未知键名、正文和嵌套敏感字段不会原样进入普通日志
        let fields =
            diagnostic_json(&serde_json::to_value(request.fields).map_err(|_| super::invalid())?);
        // 标识的字符形状不能证明内容安全；事件名也只保留摘要
        let event = body_fingerprint(request.event.as_bytes());
        let plugin_id = self.plugin_id.clone();
        let plugin_version = self.plugin_version.clone();
        let context = context.clone();
        // tracing 后端可能同步阻塞
        // 任务容量跨实例与代次共享，并由写入任务持有到完成，
        // 父调用取消不能释放容量后继续无限排队；已受理日志可在父调用结束后落盘
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            macro_rules! record {
            ($level:expr) => {
                tracing::event!(target: "gateway_plugin", $level,
                    plugin_id = %plugin_id,
                    plugin_version = %plugin_version,
                    instance_id = %context.instance_id,
                    generation = context.generation,
                    incarnation = %context.incarnation,
                    call_id = context.call_id,
                    request_id = context.request_id.as_deref(),
                    attempt_id = context.attempt_id.as_deref(),
                    stage = ?context.stage,
                    event = %event,
                    fields = %fields,
                    suppressed_logs = suppressed,
                    "插件事件"
                )
            };
        }
            match request.level {
                LogLevel::Debug => record!(tracing::Level::DEBUG),
                LogLevel::Info => record!(tracing::Level::INFO),
                LogLevel::Warn => record!(tracing::Level::WARN),
                LogLevel::Error => record!(tracing::Level::ERROR),
            }
        });
        reply(true)
    }
}

fn reply(recorded: bool) -> Result<RpcReply, PluginFault> {
    Ok(RpcReply {
        result: serde_json::to_value(LogResult { recorded }).map_err(|_| super::invalid())?,
        payload: vec![],
    })
}
