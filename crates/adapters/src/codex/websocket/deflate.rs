//! WebSocket permessage-deflate 处理。

use std::{
    collections::VecDeque,
    io::{self, Read},
    pin::Pin,
    task::{Context, Poll},
};

use flate2::read::DeflateDecoder;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const PERMESSAGE_DEFLATE_TAIL: [u8; 4] = [0x00, 0x00, 0xff, 0xff];
const OPCODE_CONTINUATION: u8 = 0x0;
const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;
const SERVER_FRAME_CHUNK_BYTES: usize = 8192;

/// Async stream wrapper that inflates server permessage-deflate data frames
/// before tungstenite sees them.
pub(crate) struct PerMessageDeflateStream<S> {
    inner: S,
    enabled: bool,
    raw_input: Vec<u8>,
    decoded_input: VecDeque<u8>,
    compressed_message: Option<CompressedMessage>,
}

struct CompressedMessage {
    opcode: u8,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameParts {
    fin: bool,
    rsv1: bool,
    opcode: u8,
    masked: bool,
    payload_offset: usize,
    payload_len: usize,
    frame_len: usize,
}

impl<S> PerMessageDeflateStream<S> {
    pub(crate) fn new(inner: S, enabled: bool, preloaded: Vec<u8>) -> Self {
        Self {
            inner,
            enabled,
            raw_input: preloaded,
            decoded_input: VecDeque::new(),
            compressed_message: None,
        }
    }
}

impl<S> AsyncRead for PerMessageDeflateStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.enabled {
            return Pin::new(&mut this.inner).poll_read(cx, buffer);
        }

        loop {
            if copy_decoded_input(&mut this.decoded_input, buffer) {
                return Poll::Ready(Ok(()));
            }

            match this.rewrite_next_frame() {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => return Poll::Ready(Err(error)),
            }

            let mut chunk = [0_u8; SERVER_FRAME_CHUNK_BYTES];
            let mut read_buffer = ReadBuf::new(&mut chunk);
            match Pin::new(&mut this.inner).poll_read(cx, &mut read_buffer) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {
                    let filled = read_buffer.filled();
                    if filled.is_empty() {
                        return Poll::Ready(Ok(()));
                    }
                    this.raw_input.extend_from_slice(filled);
                }
            }
        }
    }
}

impl<S> AsyncWrite for PerMessageDeflateStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, data)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl<S> PerMessageDeflateStream<S> {
    fn rewrite_next_frame(&mut self) -> io::Result<bool> {
        let Some(frame) = parse_frame_parts(&self.raw_input)? else {
            return Ok(false);
        };
        let raw_frame = self.raw_input.drain(..frame.frame_len).collect::<Vec<_>>();
        let payload = &raw_frame[frame.payload_offset..frame.payload_offset + frame.payload_len];

        if frame.masked {
            self.decoded_input.extend(raw_frame);
            return Ok(true);
        }

        if frame.rsv1 && is_initial_data_frame(frame.opcode) {
            if frame.fin {
                let inflated = inflate_permessage_deflate_message(payload)?;
                self.decoded_input
                    .extend(encode_server_frame(frame.opcode, false, &inflated));
            } else {
                self.compressed_message = Some(CompressedMessage {
                    opcode: frame.opcode,
                    payload: payload.to_vec(),
                });
            }
            return Ok(true);
        }

        if frame.opcode == OPCODE_CONTINUATION {
            if let Some(message) = self.compressed_message.as_mut() {
                message.payload.extend_from_slice(payload);
                if frame.fin {
                    let message = self.compressed_message.take().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "missing compressed message")
                    })?;
                    let inflated = inflate_permessage_deflate_message(&message.payload)?;
                    self.decoded_input.extend(encode_server_frame(
                        message.opcode,
                        false,
                        &inflated,
                    ));
                }
                return Ok(true);
            }
        }

        self.decoded_input.extend(raw_frame);
        Ok(true)
    }
}

fn copy_decoded_input(decoded_input: &mut VecDeque<u8>, buffer: &mut ReadBuf<'_>) -> bool {
    if decoded_input.is_empty() || buffer.remaining() == 0 {
        return false;
    }

    let copy_len = decoded_input.len().min(buffer.remaining());
    let bytes = decoded_input.drain(..copy_len).collect::<Vec<_>>();
    buffer.put_slice(&bytes);
    true
}

/// 判断扩展响应是否启用了 permessage-deflate。
pub fn permessage_deflate_enabled(extension_header: &str) -> bool {
    extension_header
        .split(',')
        .any(|value| value.trim().starts_with("permessage-deflate"))
}

/// 解压单条 permessage-deflate 消息 payload。
pub fn inflate_permessage_deflate_message(payload: &[u8]) -> io::Result<Vec<u8>> {
    let mut compressed = Vec::with_capacity(payload.len() + PERMESSAGE_DEFLATE_TAIL.len());
    compressed.extend_from_slice(payload);
    compressed.extend_from_slice(&PERMESSAGE_DEFLATE_TAIL);

    let mut decoder = DeflateDecoder::new(compressed.as_slice());
    let mut inflated = Vec::new();
    decoder.read_to_end(&mut inflated)?;
    if inflated.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "permessage-deflate frame produced no output",
        ));
    }
    Ok(inflated)
}

/// 将启用 RSV1 的压缩服务端 data frame 重写为普通未压缩 frame。
pub fn rewrite_permessage_deflate_server_frame(frame: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let Some(parts) = parse_frame_parts(frame)? else {
        return Ok(None);
    };
    if parts.frame_len != frame.len() || parts.masked || !parts.rsv1 || !parts.fin {
        return Ok(None);
    }
    if !is_data_frame(parts.opcode) {
        return Ok(None);
    }

    let payload = &frame[parts.payload_offset..parts.payload_offset + parts.payload_len];
    let inflated = inflate_permessage_deflate_message(payload)?;
    Ok(Some(encode_server_frame(parts.opcode, false, &inflated)))
}

fn parse_frame_parts(bytes: &[u8]) -> io::Result<Option<FrameParts>> {
    if bytes.len() < 2 {
        return Ok(None);
    }

    let first = bytes[0];
    let second = bytes[1];
    let mut offset = 2;
    let payload_len = match second & 0x7f {
        len @ 0..=125 => usize::from(len),
        126 => {
            if bytes.len() < offset + 2 {
                return Ok(None);
            }
            let len = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
            offset += 2;
            usize::from(len)
        }
        127 => {
            if bytes.len() < offset + 8 {
                return Ok(None);
            }
            let len = u64::from_be_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]);
            offset += 8;
            usize::try_from(len).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "websocket frame is too large")
            })?
        }
        _ => unreachable!("websocket length marker is masked to 7 bits"),
    };
    let masked = second & 0x80 != 0;
    if masked {
        offset += 4;
    }
    let frame_len = offset
        .checked_add(payload_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "websocket frame overflow"))?;
    if bytes.len() < frame_len {
        return Ok(None);
    }

    Ok(Some(FrameParts {
        fin: first & 0x80 != 0,
        rsv1: first & 0x40 != 0,
        opcode: first & 0x0f,
        masked,
        payload_offset: offset,
        payload_len,
        frame_len,
    }))
}

fn encode_server_frame(opcode: u8, rsv1: bool, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    let rsv1_bit = if rsv1 { 0x40 } else { 0 };
    frame.push(0x80 | rsv1_bit | opcode);
    match payload.len() {
        len @ 0..=125 => frame.push(len as u8),
        len @ 126..=65_535 => {
            frame.push(126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            frame.push(127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
}

fn is_data_frame(opcode: u8) -> bool {
    matches!(opcode, OPCODE_TEXT | OPCODE_BINARY | OPCODE_CONTINUATION)
}

fn is_initial_data_frame(opcode: u8) -> bool {
    matches!(opcode, OPCODE_TEXT | OPCODE_BINARY)
}
