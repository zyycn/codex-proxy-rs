//! xAI 请求画像、版本选择与官方发行资料入口

mod release;
pub mod selection;
mod state;

pub use release::{
    GROK_CLI_RELEASE_URL, GrokCliReleaseError, GrokCliReleaseService, GrokCliReleaseSnapshot,
    GrokCliReleaseStatus, GrokCliReleaseTransport,
};
pub use state::{XaiWireProfile, XaiWireProfileState};

pub(crate) use release::{GROK_CLI_RELEASE_POLL_INTERVAL, OfficialGrokCliReleaseTransport};
