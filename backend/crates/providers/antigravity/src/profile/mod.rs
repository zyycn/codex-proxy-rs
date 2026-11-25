//! 画像配置、已冻结的出站身份与发布资料的本地解析入口

pub(crate) mod archive;
pub(crate) mod artifact;
pub(crate) mod release;
pub(crate) mod selection;
pub(crate) mod verification;
mod view;
use gateway_admin::ports::provider::ProviderAdminError;
use gateway_core::account::OpaqueProviderData;
pub(crate) use release::{ReleaseService, ReleaseTask};
pub(crate) use selection::{ClientKind, ClientProfile, Platform, Selection, VersionMode, object};
pub(crate) use view::{dashboard, options, preview};

pub(crate) fn resolve(
    configuration: &OpaqueProviderData,
    releases: &ReleaseService,
) -> Result<ClientProfile, ProviderAdminError> {
    let selection = Selection::parse(configuration)?;
    Ok(selection.resolve(
        releases
            .snapshot(selection.target())
            .version(selection.client),
    ))
}
pub(crate) fn default_profile(releases: &ReleaseService) -> ClientProfile {
    let selection = Selection::default();
    selection.resolve(
        releases
            .snapshot(selection.target())
            .version(selection.client),
    )
}
