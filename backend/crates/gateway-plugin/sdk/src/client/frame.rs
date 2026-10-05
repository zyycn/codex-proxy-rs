//! 插件消息帧的异步读写与元数据、载荷长度校验

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{Frame, FrameError};

const MAX_METADATA_BYTES: usize = 64 * 1024;
const IO_CHUNK_BYTES: usize = 256 * 1024;

/// 读取完整消息；I/O 分块是传输细节，不限制业务正文总量
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame, FrameError> {
    let metadata_len = reader.read_u32().await? as usize;
    let payload_len = usize::try_from(reader.read_u64().await?).map_err(|_| FrameError::Length)?;
    check_metadata_length(metadata_len)?;
    let mut metadata = vec![0; metadata_len];
    reader.read_exact(&mut metadata).await?;
    let message = serde_json::from_slice(&metadata).map_err(|_| FrameError::Metadata)?;
    let mut payload = Vec::new();
    // 按实际读取进度分配，避免仅凭对端声明的大长度立即占满内存
    while payload.len() < payload_len {
        let amount = (payload_len - payload.len()).min(IO_CHUNK_BYTES);
        payload
            .try_reserve(amount)
            .map_err(|_| FrameError::Io(std::io::Error::from(std::io::ErrorKind::OutOfMemory)))?;
        let start = payload.len();
        payload.resize(start + amount, 0);
        reader.read_exact(&mut payload[start..]).await?;
    }
    Ok(Frame { message, payload })
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> Result<(), FrameError> {
    let metadata = serde_json::to_vec(&frame.message).map_err(|_| FrameError::Metadata)?;
    check_metadata_length(metadata.len())?;
    let metadata_len = u32::try_from(metadata.len()).map_err(|_| FrameError::Length)?;
    let payload_len = u64::try_from(frame.payload.len()).map_err(|_| FrameError::Length)?;
    writer.write_u32(metadata_len).await?;
    writer.write_u64(payload_len).await?;
    writer.write_all(&metadata).await?;
    for chunk in frame.payload.chunks(IO_CHUNK_BYTES) {
        writer.write_all(chunk).await?;
    }
    writer.flush().await?;
    Ok(())
}

/// 入队前校验本地元数据，避免单次编码失败中断共享传输
pub fn validate_frame(frame: &Frame) -> Result<(), FrameError> {
    let metadata = serde_json::to_vec(&frame.message).map_err(|_| FrameError::Metadata)?;
    check_metadata_length(metadata.len())
}

fn check_metadata_length(metadata: usize) -> Result<(), FrameError> {
    if metadata == 0 || metadata > MAX_METADATA_BYTES {
        return Err(FrameError::Length);
    }
    Ok(())
}
