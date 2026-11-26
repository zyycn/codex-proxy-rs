//! Chat 与 Responses 的直接请求和响应映射

mod request;
mod response;

pub use request::convert_request;
pub use response::{ChatStreamEncoder, project_response};
