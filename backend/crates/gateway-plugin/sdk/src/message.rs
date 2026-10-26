//! 插件双向 RPC 控制消息、流消息与二进制载荷帧的线协议定义

use serde::{Deserialize, Serialize};

use crate::{CallBudgets, CallContext, Handshake, PluginFault};

pub const PROTOCOL_VERSION: u32 = 7;

/// 元数据与二进制载荷分开，流分块不经过 JSON/base64
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Hello {
        handshake: Handshake,
    },
    Ready {
        protocol_version: u32,
        incarnation: String,
        call_budgets: CallBudgets,
    },
    Call {
        id: u64,
        method: String,
        context: CallContext,
        // 业务 JSON 由帧编码器放入载荷前缀，不占用控制元数据预算
        #[serde(skip)]
        params: serde_json::Value,
    },
    Result {
        id: u64,
        #[serde(skip)]
        result: serde_json::Value,
    },
    Error {
        id: u64,
        error: PluginFault,
    },
    Callback {
        id: u64,
        parent_id: u64,
        method: String,
        #[serde(skip)]
        params: serde_json::Value,
    },
    /// 父处理任务已交还执行权，epoch 在同一调用内严格递增
    Suspend {
        id: u64,
        epoch: u64,
    },
    /// 已挂起任务被唤醒，请求重新取得执行名额
    Resume {
        id: u64,
        epoch: u64,
    },
    /// 宿主已取得执行名额，插件才可再次驱动父处理任务
    Resumed {
        id: u64,
        epoch: u64,
    },
    /// 回调 future 已消费结果或被释放，宿主可回收对应驻留额度
    CallbackConsumed {
        id: u64,
        parent_id: u64,
    },
    Cancel {
        id: u64,
    },
    Cancelled {
        id: u64,
    },
    Stream {
        id: u64,
        sequence: u64,
    },
    Credit {
        id: u64,
        bytes: u32,
        frames: u32,
    },
    End {
        id: u64,
        error: Option<PluginFault>,
    },
    Quiesce,
    Shutdown,
}

#[cfg(feature = "io")]
impl Message {
    pub(crate) const fn data(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Hello { handshake } => Some(&handshake.configuration),
            Self::Call { params, .. } | Self::Callback { params, .. } => Some(params),
            Self::Result { result, .. } => Some(result),
            _ => None,
        }
    }

    pub(crate) const fn data_mut(&mut self) -> Option<&mut serde_json::Value> {
        match self {
            Self::Hello { handshake } => Some(&mut handshake.configuration),
            Self::Call { params, .. } | Self::Callback { params, .. } => Some(params),
            Self::Result { result, .. } => Some(result),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Frame {
    pub message: Message,
    pub payload: Vec<u8>,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 配置、调用参数和结果都可能包含凭据；诊断仅暴露消息类别及关联 ID
        let (kind, id) = match self {
            Self::Hello { .. } => ("Hello", None),
            Self::Ready { .. } => ("Ready", None),
            Self::Call { id, .. } => ("Call", Some(id)),
            Self::Result { id, .. } => ("Result", Some(id)),
            Self::Error { id, .. } => ("Error", Some(id)),
            Self::Callback { id, .. } => ("Callback", Some(id)),
            Self::Suspend { id, .. } => ("Suspend", Some(id)),
            Self::Resume { id, .. } => ("Resume", Some(id)),
            Self::Resumed { id, .. } => ("Resumed", Some(id)),
            Self::CallbackConsumed { id, .. } => ("CallbackConsumed", Some(id)),
            Self::Cancel { id } => ("Cancel", Some(id)),
            Self::Cancelled { id } => ("Cancelled", Some(id)),
            Self::Stream { id, .. } => ("Stream", Some(id)),
            Self::Credit { id, .. } => ("Credit", Some(id)),
            Self::End { id, .. } => ("End", Some(id)),
            Self::Quiesce => ("Quiesce", None),
            Self::Shutdown => ("Shutdown", None),
        };
        formatter
            .debug_struct(kind)
            .field("id", &id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Frame")
            .field("message", &self.message)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

impl Frame {
    #[must_use]
    pub fn control(message: Message) -> Self {
        Self {
            message,
            payload: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("plugin frame exceeds its limit or has invalid lengths")]
    Length,
    #[error("plugin frame metadata is invalid")]
    Metadata(#[source] serde_json::Error),
    #[error("plugin transport is closed or incomplete")]
    Io(#[from] std::io::Error),
}
