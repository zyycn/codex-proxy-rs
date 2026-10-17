//! Provider 运行时所需的存储合同，按能力组织端口与共享策略

mod error;
pub use error::{ProviderStoreError, ProviderStoreErrorKind};
mod lease;
pub use lease::{
    ProviderConcurrencyPool, ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeaseMetadata,
    ProviderLeasePort, ProviderLeaseRequest, ProviderRefreshCapacityRequest,
    ProviderRefreshLeaseRequest, ProviderSchedulingLeaseRequest, ProviderSchedulingState,
};
mod affinity;
pub use affinity::{
    ProviderSessionAffinityKey, ProviderSessionAffinityPort, ProviderSessionAlias,
    ProviderSessionBinding, ProviderSessionExclusionPort, ProviderSessionExclusions,
};
mod catalog;
pub use catalog::{
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderCatalogCacheKey,
    ProviderCatalogCachePort, ProviderCatalogScope,
};
mod credential;
pub use credential::{ProviderCredentialState, ProviderCredentialStatePort};
mod cooldown;
pub use cooldown::{
    ProviderCooldown, ProviderCooldownKind, ProviderCooldownPort, ProviderCooldownScope,
    ProviderScopedCooldown,
};
mod refresh;
pub use refresh::{ProviderRefreshPolicy, provider_refresh_backoff_at, provider_refresh_retry_at};
mod runtime_policy;
pub use runtime_policy::{
    ProviderFreezePolicy, ProviderRuntimePolicyPort, ProviderWarmupPolicy,
    valid_warmup_schedule_time,
};
mod oauth;
pub use oauth::{
    NewOAuthPendingFlow, OAuthPendingBinding, OAuthPendingClaimOutcome, OAuthPendingConsumeOutcome,
    OAuthPendingFlowPort, OAuthPendingPutOutcome, OAuthPendingReleaseOutcome,
};
mod bundle;
pub use bundle::ProviderStorePorts;
