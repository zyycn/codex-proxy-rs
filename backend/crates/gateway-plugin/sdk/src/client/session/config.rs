//! 插件侧会话的内存、并发与期限配置

use std::time::Duration;

use super::SessionError;

const MAXIMUM_STREAM_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_CONCURRENCY: usize = 256;
const MAXIMUM_BUFFERED_STREAM_CHUNKS: usize = 65_536;

/// 插件侧会话的内存、并发与期限边界
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    /// 单个业务流分块的预算，不限制普通调用正文的总长度
    pub maximum_stream_chunk_bytes: usize,
    pub maximum_calls: usize,
    pub maximum_callbacks: usize,
    pub maximum_buffered_stream_chunks: usize,
    pub handshake_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            // 流分块还需匹配宿主授予的信用窗口
            maximum_stream_chunk_bytes: 1024 * 1024,
            maximum_calls: 256,
            maximum_callbacks: 64,
            maximum_buffered_stream_chunks: 1_024,
            handshake_timeout: Duration::from_secs(5),
        }
    }
}

impl SessionConfig {
    pub(super) fn validate(self) -> Result<Self, SessionError> {
        if self.maximum_stream_chunk_bytes < 1_024
            || self.maximum_stream_chunk_bytes > MAXIMUM_STREAM_CHUNK_BYTES
            || self.maximum_calls == 0
            || self.maximum_calls > MAXIMUM_CONCURRENCY
            || self.maximum_callbacks == 0
            || self.maximum_callbacks > MAXIMUM_CONCURRENCY
            || self.maximum_buffered_stream_chunks == 0
            || self.maximum_buffered_stream_chunks > MAXIMUM_BUFFERED_STREAM_CHUNKS
            || self.handshake_timeout.is_zero()
        {
            return Err(SessionError::Configuration);
        }
        Ok(self)
    }
}
