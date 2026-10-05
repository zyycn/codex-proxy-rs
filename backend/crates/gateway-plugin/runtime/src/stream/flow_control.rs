//! 插件流的字节与帧数窗口校验、序号推进及信用归还

use crate::RpcError;

/// 字节与帧数一起授予，避免攻击者用大量单字节分块突破队列上限
pub(super) struct ReceiveWindow {
    bytes: u32,
    frames: u32,
    next_sequence: u64,
}

impl ReceiveWindow {
    pub fn new(bytes: u32, frames: u32) -> Self {
        Self {
            bytes,
            frames,
            next_sequence: 0,
        }
    }

    pub fn receive(&mut self, sequence: u64, bytes: usize) -> Result<(), RpcError> {
        let bytes = u32::try_from(bytes).map_err(|_| RpcError::Protocol)?;
        if sequence != self.next_sequence || bytes == 0 || bytes > self.bytes || self.frames == 0 {
            return Err(RpcError::Protocol);
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(RpcError::Protocol)?;
        self.bytes -= bytes;
        self.frames -= 1;
        Ok(())
    }

    pub fn release(&mut self, bytes: u32) -> Result<(), RpcError> {
        self.bytes = self.bytes.checked_add(bytes).ok_or(RpcError::Protocol)?;
        self.frames = self.frames.checked_add(1).ok_or(RpcError::Protocol)?;
        Ok(())
    }
}
