//! OpenAI/Codex Images JSON 透传与图片上传适配

mod http;
mod request;

pub(crate) use http::{image_edits, image_generations};
