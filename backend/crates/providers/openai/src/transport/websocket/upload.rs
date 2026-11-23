//! 上传 response.create 时同时驱动读写，保留与本次上传有关的上游回复

use std::{
    future::poll_fn,
    task::{Context, Poll, ready},
};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::{sync::mpsc, time::Instant};
use tungstenite::{
    Error, Message, Utf8Bytes,
    protocol::frame::{Fragment, Frame, coding::Data},
};

use super::{
    CodexWebSocketExchangeError,
    pump::{PumpExitReason, RawWsStream},
};

const FRAGMENT_BYTES: usize = 1024 * 1024;
const READ_AHEAD_BYTES: usize = 1024 * 1024;
const READ_BATCH: usize = 32;

pub(super) fn text_frames(text: Utf8Bytes) -> impl Iterator<Item = Message> {
    let (single, mut remaining) = if text.len() <= FRAGMENT_BYTES {
        (Some(Message::Text(text)), Bytes::new())
    } else {
        (None, Bytes::from(text))
    };
    let mut opcode = Data::Text;
    single.into_iter().chain(std::iter::from_fn(move || {
        if remaining.is_empty() {
            return None;
        }
        let payload = remaining.split_to(remaining.len().min(FRAGMENT_BYTES));
        let fragment = if remaining.is_empty() {
            Fragment::Final
        } else {
            Fragment::More
        };
        let frame = Frame::message_fragment(payload, opcode, fragment);
        opcode = Data::Continue;
        Some(Message::Frame(frame))
    }))
}

pub(super) struct Upload<'a> {
    messages: &'a mpsc::Sender<Result<Message, CodexWebSocketExchangeError>>,
    budget: usize,
    pub(super) last_activity: Option<Instant>,
    pub(super) started: bool,
    pub(super) received_reply: bool,
    pub(super) close: Option<PumpExitReason>,
    queued_pongs: usize,
}

impl<'a> Upload<'a> {
    pub(super) fn new(
        messages: &'a mpsc::Sender<Result<Message, CodexWebSocketExchangeError>>,
        backpressured: bool,
    ) -> Self {
        Self {
            messages,
            budget: if backpressured { 0 } else { READ_AHEAD_BYTES },
            last_activity: None,
            started: false,
            received_reply: false,
            close: None,
            queued_pongs: 0,
        }
    }

    fn poll_incoming(
        &mut self,
        socket: &mut RawWsStream,
        cx: &mut Context<'_>,
        recovering: bool,
    ) -> Poll<Result<(), Error>> {
        if !recovering && self.queued_pongs > 0 && socket.poll_flush_unpin(cx)?.is_ready() {
            self.queued_pongs = 0;
        }
        for _ in 0..READ_BATCH {
            if self.budget == 0 || self.messages.capacity() == 0 {
                return Poll::Ready(Ok(()));
            }
            if !recovering && self.queued_pongs >= READ_BATCH {
                return Poll::Pending;
            }
            let next = socket.poll_next_unpin(cx);
            if recovering && next.is_pending() {
                return Poll::Ready(Ok(()));
            }
            let message = ready!(next).transpose()?.ok_or(Error::ConnectionClosed)?;
            if !matches!(message, Message::Close(_)) {
                self.last_activity = Some(Instant::now());
            }
            if recovering || !matches!(message, Message::Ping(_) | Message::Pong(_)) {
                self.budget = self
                    .budget
                    .saturating_sub(message.len() + size_of::<Message>());
            }
            match message {
                Message::Ping(_) => {
                    // tungstenite 已排入自动 Pong，在下一个数据分片前驱动控制帧写出
                    match socket.poll_flush_unpin(cx) {
                        Poll::Ready(Ok(())) => self.queued_pongs = 0,
                        Poll::Ready(Err(error)) if !recovering => return Poll::Ready(Err(error)),
                        _ => self.queued_pongs += 1,
                    }
                }
                Message::Pong(_) => {}
                message => {
                    self.close = PumpExitReason::from_close(&message);
                    // pump 是唯一生产方；读帧前已经确认有队列容量
                    self.messages
                        .try_send(Ok(message))
                        .map_err(|_| Error::ConnectionClosed)?;
                    self.received_reply |= self.started;
                    if self.close.is_some() {
                        return Poll::Ready(Err(Error::ConnectionClosed));
                    }
                }
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    pub(super) async fn send(
        &mut self,
        socket: &mut RawWsStream,
        text: Utf8Bytes,
    ) -> Result<(), Error> {
        for frame in text_frames(text) {
            let mut frame = Some(frame);
            let mut first_poll = true;
            let result = poll_fn(|cx| {
                let read_first = std::mem::take(&mut first_poll);
                if read_first {
                    let _ = self.poll_incoming(socket, cx, false)?;
                }
                let write = (|| {
                    if frame.is_some() {
                        ready!(socket.poll_ready_unpin(cx))?;
                    }
                    if let Some(frame) = frame.take() {
                        let result = socket.start_send_unpin(frame);
                        // I/O 错误可能发生在首帧头已部分写出之后
                        self.started |= matches!(result, Ok(()) | Err(Error::Io(_)));
                        result?;
                    }
                    socket.poll_flush_unpin(cx)
                })();
                if !read_first && write.is_pending() {
                    let _ = self.poll_incoming(socket, cx, false)?;
                }
                write
            })
            .await;
            if let Err(error) = result {
                if self.started && !matches!(error, Error::ConnectionClosed) {
                    // 只取已就绪的回复，不能为了等待假设的 Close 延长写入期限
                    let _ = poll_fn(|cx| self.poll_incoming(socket, cx, true)).await;
                }
                return Err(error);
            }
        }
        Ok(())
    }
}
