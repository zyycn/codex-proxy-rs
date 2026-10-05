//! 插件 SDK 线协议、清单与客户端的测试入口

mod call;
#[cfg(feature = "io")]
mod client;
mod manifest;
mod message;
