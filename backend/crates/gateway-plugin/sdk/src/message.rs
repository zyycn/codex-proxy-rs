//! 插件双向 RPC 控制消息、流消息与二进制载荷帧的线协议定义

use serde::{Deserialize, Serialize};

use crate::{CallContext, Handshake, PluginFault};

pub const PROTOCOL_VERSION: u32 = 2;

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
    },
    Call {
        id: u64,
        method: String,
        context: CallContext,
        params: serde_json::Value,
    },
    Result {
        id: u64,
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
        params: serde_json::Value,
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
    Metadata,
    #[error("plugin transport is closed or incomplete")]
    Io(#[from] std::io::Error),
}
