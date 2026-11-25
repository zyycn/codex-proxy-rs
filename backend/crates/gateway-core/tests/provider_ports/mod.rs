//! 验证 Provider 协调端口的敏感值保护、有效期与刷新边界

mod affinity;
mod lease;
mod oauth;
mod refresh;
mod runtime_policy;
mod session_affinity;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use gateway_core::account::{
    AccountRuntimeSignals, CredentialRevision, OpaqueProviderData, ProviderAccountId,
};
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, ProviderRefreshPolicy, ProviderSchedulingState,
    ProviderSessionAffinityKey, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
