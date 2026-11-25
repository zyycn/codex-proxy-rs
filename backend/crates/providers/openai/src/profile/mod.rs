//! OpenAI 请求画像、版本选择与官方发行资料入口

mod baseline;
pub mod cli_release;
mod context;
pub mod desktop_artifact;
pub mod identity;
pub mod platform_release;
pub mod selection;

mod release;
mod state;

pub use gateway_core::account::RequestLocation as CodexRequestLocation;
pub use release::{
    APPCAST_POLL_INTERVAL, CODEX_DESKTOP_APPCAST_URL, CodexArtifactProfileCache,
    CodexBundledReleaseProfile, CodexDesktopRelease, CodexDesktopReleaseError,
    CodexDesktopReleaseService, CodexDesktopReleaseSnapshot, CodexDesktopReleaseStatus,
    CodexDesktopReleaseTransport, OfficialCodexDesktopReleaseTransport, parse_desktop_release,
};
use release::{
    ARTIFACT_PROFILE_CACHE_RENEW_INTERVAL, ARTIFACT_PROFILE_CACHE_TTL, numeric_dotted_version,
};
pub use state::{CodexWireProfile, CodexWireProfileState};
