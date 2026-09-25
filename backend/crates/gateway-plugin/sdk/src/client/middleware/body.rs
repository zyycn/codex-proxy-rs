use std::{collections::VecDeque, num::NonZeroUsize};

use crate::{
    ErrorCode, PluginFault,
    call::middleware::{
        BODY_CLOSE_METHOD, BODY_READ_METHOD, MiddlewareBodyClose, MiddlewareBodyCloseResult,
        MiddlewareBodyDisposition, MiddlewareBodyFrame, MiddlewareBodyFraming,
        MiddlewareBodyHandle, MiddlewareBodyRead, MiddlewareBodyReadResult, MiddlewareResponseBody,
    },
};

use super::super::session::{
    HostClient, PullResponseFuture, PullResponseStream, ResponseStream, SessionError, StreamSender,
};
use super::invalid_input;

const MAXIMUM_MAPPED_FRAMES_PER_SOURCE: usize = 64;
const MAXIMUM_MAPPED_BYTES_PER_SOURCE: usize = 8 * 1024 * 1024;

/// 惰性正文的一次完整 frame。
pub struct MiddlewareBody {
    source: MiddlewareBodySource,
}

enum MiddlewareBodySource {
    Empty,
    Host {
        body: MiddlewareBodyHandle,
        host: HostClient,
        touched: bool,
        last_source_id: u64,
    },
    Plugin {
        framing: MiddlewareBodyFraming,
        stream: ResponseStream,
    },
}

impl MiddlewareBody {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            source: MiddlewareBodySource::Empty,
        }
    }

    /// 从已编码完整 frame 构造有界插件输出流。
    #[must_use]
    pub fn from_frames(framing: MiddlewareBodyFraming, frames: Vec<MiddlewareBodyFrame>) -> Self {
        Self {
            source: MiddlewareBodySource::Plugin {
                framing,
                stream: ResponseStream::from_chunks(
                    frames
                        .into_iter()
                        .map(MiddlewareBodyFrame::encode)
                        .collect(),
                ),
            },
        }
    }

    /// 创建插件生产的有界正文流。
    #[must_use]
    pub fn channel(
        framing: MiddlewareBodyFraming,
        capacity: NonZeroUsize,
    ) -> (MiddlewareBodySender, Self) {
        let (sender, stream) = ResponseStream::channel(capacity);
        (
            MiddlewareBodySender { sender },
            Self {
                source: MiddlewareBodySource::Plugin { framing, stream },
            },
        )
    }

    #[must_use]
    pub const fn framing(&self) -> Option<MiddlewareBodyFraming> {
        match &self.source {
            MiddlewareBodySource::Empty => None,
            MiddlewareBodySource::Host { body, .. } => Some(body.framing),
            MiddlewareBodySource::Plugin { framing, .. } => Some(*framing),
        }
    }

    /// 拉取下一完整 frame。调用后该 opaque handle 不能再作为未读直通返回。
    /// 只读观察并原样交付时使用 [`Self::inspect_frames`]，由 SDK 保留源帧。
    pub async fn read(&mut self) -> Result<Option<MiddlewareBodyFrame>, PluginFault> {
        let MiddlewareBodySource::Host {
            body,
            host,
            touched,
            last_source_id,
        } = &mut self.source
        else {
            return Err(PluginFault::new(
                ErrorCode::InvalidInput,
                "middleware body is not a downstream handle",
            ));
        };
        *touched = true;
        let maximum_bytes =
            u32::try_from(host.maximum_stream_chunk_bytes()).map_err(|_| invalid_input())?;
        let reply = host
            .call(
                BODY_READ_METHOD,
                serde_json::to_value(MiddlewareBodyRead {
                    handle: body.handle.clone(),
                    maximum_bytes,
                })
                .map_err(|_| invalid_input())?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let result: MiddlewareBodyReadResult =
            serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        if result.framing != body.framing
            || (result.eof && result.source_id != 0)
            || (!result.eof && (result.source_id == 0 || result.source_id <= *last_source_id))
        {
            return Err(invalid_input());
        }
        if result.eof {
            if result.terminal || !reply.payload.is_empty() {
                return Err(invalid_input());
            }
            return Ok(None);
        }
        *last_source_id = result.source_id;
        MiddlewareBodyFrame::from_source(reply.payload, result.terminal, result.source_id)
            .map(Some)
            .map_err(|_| invalid_input())
    }

    /// 提前关闭宿主正文句柄。父调用取消或结束也会由 Runtime 兜底回收。
    pub async fn close(&mut self) -> Result<(), PluginFault> {
        let MiddlewareBodySource::Host { body, host, .. } = &self.source else {
            self.source = MiddlewareBodySource::Empty;
            return Ok(());
        };
        let reply = host
            .call(
                BODY_CLOSE_METHOD,
                serde_json::to_value(MiddlewareBodyClose {
                    handle: body.handle.clone(),
                })
                .map_err(|_| invalid_input())?,
                Vec::new(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if !reply.payload.is_empty()
            || serde_json::from_value::<MiddlewareBodyCloseResult>(reply.result).is_err()
        {
            return Err(invalid_input());
        }
        self.source = MiddlewareBodySource::Empty;
        Ok(())
    }

    /// 按下游 Credit 只读观察完整 frame，并原样交付其字节、顺序和源终态。
    ///
    /// 不预读或重编码正文；闭包只取得借用，不能改写正在交付的帧。
    /// 与映射共用取消和背压边界，观察工作应保持有界。
    ///
    /// # Errors
    ///
    /// 正文来自插件自建流时返回错误；空正文直接保留。
    pub fn inspect_frames<F>(self, mut inspect: F) -> Result<Self, PluginFault>
    where
        F: FnMut(&MiddlewareBodyFrame) + Send + 'static,
    {
        self.map_frames(move |frame| {
            inspect(&frame);
            Ok(vec![frame])
        })
    }

    /// 按下游 Credit 惰性拉取并映射完整 frame；闭包可在单次调用内持有状态。
    ///
    /// 每个输入可生成零个、一个或多个输出。SDK 不预读或另起后台任务；Runtime
    /// 继续复核 frame 大小、源终态与输出终态的一致性。
    pub fn map_frames<F>(self, transform: F) -> Result<Self, PluginFault>
    where
        F: FnMut(MiddlewareBodyFrame) -> Result<Vec<MiddlewareBodyFrame>, PluginFault>
            + Send
            + 'static,
    {
        let Some(framing) = self.framing() else {
            return Ok(Self::empty());
        };
        if !matches!(&self.source, MiddlewareBodySource::Host { .. }) {
            return Err(PluginFault::new(
                ErrorCode::InvalidInput,
                "only a downstream middleware body can be mapped",
            ));
        }
        Ok(Self {
            source: MiddlewareBodySource::Plugin {
                framing,
                stream: ResponseStream::pull(Box::new(MappedBody {
                    input: self,
                    transform,
                    buffered: VecDeque::new(),
                    finished: false,
                })),
            },
        })
    }

    pub(super) fn from_host(body: MiddlewareBodyHandle, host: HostClient) -> Self {
        Self {
            source: MiddlewareBodySource::Host {
                body,
                host,
                touched: false,
                last_source_id: 0,
            },
        }
    }

    pub(super) fn into_wire(self) -> Result<(MiddlewareResponseBody, ResponseStream), PluginFault> {
        match self.source {
            MiddlewareBodySource::Empty => Ok((
                MiddlewareResponseBody::Empty,
                ResponseStream::from_chunks(Vec::new()),
            )),
            MiddlewareBodySource::Host {
                body,
                touched: false,
                ..
            } => Ok((
                MiddlewareResponseBody::PassThrough { body },
                ResponseStream::from_chunks(Vec::new()),
            )),
            MiddlewareBodySource::Host { touched: true, .. } => Err(PluginFault::new(
                ErrorCode::InvalidInput,
                "a consumed middleware body cannot be passed through",
            )),
            MiddlewareBodySource::Plugin { framing, stream } => {
                Ok((MiddlewareResponseBody::Stream { framing }, stream))
            }
        }
    }
}

struct MappedBody<F> {
    input: MiddlewareBody,
    transform: F,
    buffered: VecDeque<MiddlewareBodyFrame>,
    finished: bool,
}

impl<F> PullResponseStream for MappedBody<F>
where
    F: FnMut(MiddlewareBodyFrame) -> Result<Vec<MiddlewareBodyFrame>, PluginFault> + Send + 'static,
{
    fn next(&mut self) -> PullResponseFuture<'_> {
        Box::pin(async move {
            loop {
                if let Some(frame) = self.buffered.pop_front() {
                    return Some(Ok(frame.encode()));
                }
                if self.finished {
                    return None;
                }
                let frame = match self.input.read().await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => {
                        self.finished = true;
                        return None;
                    }
                    Err(error) => {
                        self.finished = true;
                        return Some(Err(error));
                    }
                };
                let source_id = frame.source_id();
                match (self.transform)(frame) {
                    Ok(frames) => {
                        let count = frames.len();
                        let mapped_bytes = frames.iter().try_fold(0_usize, |total, frame| {
                            total.checked_add(frame.payload.len())
                        });
                        if count > MAXIMUM_MAPPED_FRAMES_PER_SOURCE
                            || mapped_bytes
                                .is_none_or(|bytes| bytes > MAXIMUM_MAPPED_BYTES_PER_SOURCE)
                        {
                            self.finished = true;
                            let _ = self.input.close().await;
                            return Some(Err(PluginFault::new(
                                ErrorCode::Capacity,
                                "middleware frame expansion exceeds the limit",
                            )));
                        }
                        if count == 0 {
                            let marker = MiddlewareBodyFrame::new(Vec::new(), false)
                                .map_to_source(source_id, MiddlewareBodyDisposition::Drop)
                                .map_err(|_| invalid_input());
                            match marker {
                                Ok(marker) => self.buffered.push_back(marker),
                                Err(error) => {
                                    self.finished = true;
                                    let _ = self.input.close().await;
                                    return Some(Err(error));
                                }
                            }
                        } else {
                            for (index, frame) in frames.into_iter().enumerate() {
                                let disposition = if count == 1 {
                                    MiddlewareBodyDisposition::Only
                                } else if index == 0 {
                                    MiddlewareBodyDisposition::First
                                } else if index + 1 == count {
                                    MiddlewareBodyDisposition::Last
                                } else {
                                    MiddlewareBodyDisposition::More
                                };
                                match frame.map_to_source(source_id, disposition) {
                                    Ok(frame) => self.buffered.push_back(frame),
                                    Err(_) => {
                                        self.finished = true;
                                        let _ = self.input.close().await;
                                        return Some(Err(invalid_input()));
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        self.finished = true;
                        let _ = self.input.close().await;
                        return Some(Err(error));
                    }
                }
            }
        })
    }
}

/// 插件输出正文的有界生产端；每个发送项必须是完整 frame。
#[derive(Clone)]
pub struct MiddlewareBodySender {
    sender: StreamSender,
}

impl MiddlewareBodySender {
    pub async fn send(&self, frame: MiddlewareBodyFrame) -> Result<(), SessionError> {
        self.sender.send(frame.encode()).await
    }

    pub async fn fail(&self, fault: PluginFault) -> Result<(), SessionError> {
        self.sender.fail(fault).await
    }
}
