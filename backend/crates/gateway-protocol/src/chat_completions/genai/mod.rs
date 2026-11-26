//! Chat 与 GenAI 的直接请求和响应映射

mod request;
mod response;
mod stream;

pub use request::convert_request;
pub use response::{project_response, project_response_with_request};
pub use stream::ChatStreamEncoder;
