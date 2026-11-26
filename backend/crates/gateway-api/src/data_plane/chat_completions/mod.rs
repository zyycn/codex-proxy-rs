//! Chat 原始协议入口与交付，目标协议转换由选定的 Provider 装配

pub(in crate::data_plane) mod delivery;
mod http;

pub(in crate::data_plane) use delivery::chat_error_frame;
pub(super) use http::chat_completions;
pub(in crate::data_plane) use http::decode_request;
