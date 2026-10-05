//! 实验性插件线协议；不依赖网关领域或基础设施类型

pub mod call;
mod capability;
#[cfg(feature = "io")]
pub mod client;
mod context;
mod error;
mod manifest;
mod message;

pub use capability::{Capability, ContributionDeclaration, Contributions, FailurePolicy, Stage};
pub use context::{CallContext, Handshake};
pub use error::{ErrorCode, PluginFault, SendState};
pub use manifest::{
    Engines, MANIFEST_VERSION, Manifest, ManifestError, Package, PackageTarget, PluginIcon,
    PluginIconVariants, RuntimeKind, StateNamespace, valid_package_path,
};
pub use message::{Frame, FrameError, Message, PROTOCOL_VERSION};
