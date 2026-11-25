//! xAI 管理边界的模块门面

mod credentials;
mod error;
mod pending;
mod profile;
mod provider;
mod quota;

pub(crate) use provider::{XaiAdminProvider, XaiAdminServices};
