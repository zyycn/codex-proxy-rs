//! 验证 xAI 管理端公开的额度能力

use super::*;

#[tokio::test]
async fn account_capabilities_advertise_only_xai_oauth_quota_operations() {
    let bundle = provider_xai::initialize(provider_ports()).await.unwrap();
    let provider = bundle.admin.clone();
    let id = ProviderAccountId::new("acct_capabilities").unwrap();
    let oauth = provider.account_capabilities(&id, "oauth");
    assert_eq!(
        oauth,
        gateway_admin::model::accounts::ProviderAccountCapabilities {
            credential_refresh: true,
            reauthorize: true,
            credential_export: true,
            quota: true,
            quota_refresh: true,
            ..Default::default()
        }
    );
    for kind in ["api_key", "unknown"] {
        assert_eq!(provider.account_capabilities(&id, kind), Default::default());
    }
}
