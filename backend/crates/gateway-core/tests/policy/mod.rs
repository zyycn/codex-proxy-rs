//! 验证 Client Key 准入、限额、凭据保护与客户端版本策略

mod client_version;
mod contract;
use std::sync::Arc;

use gateway_core::policy::{
    ClientApiKeyId, ClientPolicy, ClientVersionRejection, CodexClientKind, CodexClientMinVersions,
    CodexClientVersion, PlaintextClientApiKey, RateLimits,
};
use gateway_core::routing::{ClientRoutingScope, FrozenAccountScope, RuntimeAccountDirectory};

fn plaintext(value: &str) -> PlaintextClientApiKey {
    PlaintextClientApiKey::new(value).expect("valid plaintext client key")
}

fn account_scope() -> Arc<FrozenAccountScope> {
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::default()),
        ClientRoutingScope::all_accounts(),
    ))
}
