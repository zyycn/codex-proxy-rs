//! 复用共享制品缓存，核验事实必须绑定完整发布资料和目标平台

use super::{
    artifact::{self, Release},
    selection::ReleaseTarget,
};
use chrono::{DateTime, Utc};
use gateway_core::{
    account::OpaqueProviderData,
    provider_ports::{ProviderArtifactProfile, ProviderArtifactProfileCachePort},
    routing::ProviderKind,
    task::WorkerTaskError,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const CACHE_TTL: Duration = Duration::from_secs(30 * 86400);
const RENEW_INTERVAL: Duration = Duration::from_secs(86400);

#[derive(Clone)]
pub(crate) struct VerifiedArtifact {
    pub(crate) release: Release,
    pub(crate) verified_at: DateTime<Utc>,
    renewed_at: Instant,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    target: ReleaseTarget,
    release: Release,
}

pub(crate) struct VerificationCache {
    provider: ProviderKind,
    cache: Arc<dyn ProviderArtifactProfileCachePort>,
}
impl VerificationCache {
    pub(crate) fn new(
        provider: ProviderKind,
        cache: Arc<dyn ProviderArtifactProfileCachePort>,
    ) -> Self {
        Self { provider, cache }
    }

    pub(crate) async fn verify(
        &self,
        client: &reqwest::Client,
        target: ReleaseTarget,
        release: &Release,
        previous: Option<&VerifiedArtifact>,
    ) -> Result<VerifiedArtifact, WorkerTaskError> {
        let sequence = sequence(&release.version)?;
        if let Some(previous) = previous.filter(|previous| previous.release == *release)
            && previous.renewed_at.elapsed() < RENEW_INTERVAL
        {
            return Ok(previous.clone());
        }
        let key = key(target);
        let cached = self
            .cache
            .read(&self.provider, &key)
            .await
            .map_err(|e| error().with_source(e))?;
        let verified_at = if let Some(cached) = cached {
            let payload: Payload = serde_json::from_value(serde_json::Value::Object(
                cached.profile().expose_to_provider().clone(),
            ))
            .map_err(|e| error().with_source(e))?;
            if cached.provider_kind() != &self.provider
                || cached.artifact_key() != key
                || payload.target != target
                || cached.artifact_sequence() != self::sequence(&payload.release.version)?
            {
                return Err(error());
            }
            if cached.artifact_sequence() > sequence
                || (cached.artifact_sequence() == sequence && payload.release != *release)
            {
                return Err(error());
            }
            (payload.release == *release).then(|| DateTime::<Utc>::from(cached.verified_at()))
        } else {
            None
        };
        let verified_at = match verified_at.or_else(|| {
            previous
                .filter(|v| v.release == *release)
                .map(|v| v.verified_at)
        }) {
            Some(at) => at,
            None => {
                artifact::verify(
                    client,
                    target,
                    &release.version,
                    &release.url,
                    release.sha512.as_deref(),
                )
                .await?;
                Utc::now()
            }
        };
        let payload = serde_json::to_value(Payload {
            target,
            release: release.clone(),
        })
        .map_err(|e| error().with_source(e))?;
        let serde_json::Value::Object(payload) = payload else {
            return Err(error());
        };
        let profile = ProviderArtifactProfile::new(
            self.provider.clone(),
            key,
            sequence,
            verified_at.into(),
            OpaqueProviderData::new(payload),
        );
        if !self
            .cache
            .replace_if_newer(profile, CACHE_TTL)
            .await
            .map_err(|e| error().with_source(e))?
        {
            return Err(error());
        }
        Ok(VerifiedArtifact {
            release: release.clone(),
            verified_at,
            renewed_at: Instant::now(),
        })
    }
}

pub(crate) fn key(target: ReleaseTarget) -> String {
    let client = match target.client {
        super::selection::ClientKind::Desktop => "desktop",
        super::selection::ClientKind::Cli => "cli",
    };
    format!(
        "{client}-{}-{}",
        target.platform.os_type(),
        target.arch.label()
    )
}

pub(crate) fn sequence(version: &str) -> Result<u64, WorkerTaskError> {
    let parts: Vec<u16> = version
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| error())?;
    if parts.len() != 3 {
        return Err(error());
    }
    Ok(u64::from(parts[0]) << 32 | u64::from(parts[1]) << 16 | u64::from(parts[2]))
}
fn error() -> WorkerTaskError {
    WorkerTaskError::safe("Antigravity 已核验发布包缓存无效或不可用")
}
