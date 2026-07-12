//! 运行时设置校验、持久化与当前快照。

use std::{collections::BTreeMap, sync::Arc};

use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, watch};

use crate::infra::identity::{generate_admin_api_key, hash_credential};

use super::{
    store::PgSettingsStore,
    types::{
        ManagementApiKeyStatus, SettingsError, SettingsPatch, SettingsSnapshot,
        SettingsValidationError,
    },
};

const ROTATION_STRATEGIES: [&str; 4] = ["smart", "quota_reset_priority", "round_robin", "sticky"];

/// 持久化设置的当前快照。
#[derive(Clone)]
pub struct SettingsService {
    store: PgSettingsStore,
    sender: watch::Sender<SettingsSnapshot>,
    update_lock: Arc<Mutex<()>>,
}

impl SettingsService {
    pub fn apply_patch(
        current: &mut SettingsSnapshot,
        patch: SettingsPatch,
    ) -> Result<(), SettingsValidationError> {
        if let Some(model_aliases) = patch.model_aliases {
            current.model_aliases = validate_model_aliases(model_aliases)?;
        }
        if let Some(value) = patch.refresh_margin_seconds {
            current.refresh_margin_seconds = positive_u64("refreshMarginSeconds", value)?;
        }
        if let Some(value) = patch.refresh_concurrency {
            current.refresh_concurrency = positive_u32("refreshConcurrency", value)?;
        }
        if let Some(value) = patch.max_concurrent_per_account {
            current.max_concurrent_per_account = positive_usize("maxConcurrentPerAccount", value)?;
        }
        if let Some(value) = patch.request_interval_ms {
            current.request_interval_ms = value;
        }
        if let Some(value) = patch.rotation_strategy {
            current.rotation_strategy = validate_rotation_strategy(&value)?;
        }
        Ok(())
    }

    pub fn new(settings: SettingsSnapshot, pool: sqlx::PgPool) -> Self {
        let (sender, _) = watch::channel(settings);
        Self {
            store: PgSettingsStore::new(pool),
            sender,
            update_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn load_or_initialize(
        defaults: SettingsSnapshot,
        pool: &sqlx::PgPool,
    ) -> Result<SettingsSnapshot, SettingsError> {
        PgSettingsStore::new(pool.clone())
            .load_or_initialize(&defaults)
            .await
    }

    pub fn current(&self) -> SettingsSnapshot {
        self.sender.borrow().clone()
    }

    /// 订阅设置快照变更。
    pub fn subscribe(&self) -> watch::Receiver<SettingsSnapshot> {
        self.sender.subscribe()
    }

    pub async fn update(&self, patch: SettingsPatch) -> Result<SettingsSnapshot, SettingsError> {
        let _guard = self.update_lock.lock().await;
        let mut next = self.current();
        SettingsService::apply_patch(&mut next, patch)?;
        self.store.save(&next).await?;
        self.sender.send_replace(next.clone());
        Ok(next)
    }

    pub async fn admin_api_key_status(&self) -> Result<ManagementApiKeyStatus, SettingsError> {
        self.store.admin_api_key_status().await
    }

    pub async fn regenerate_admin_api_key(&self) -> Result<String, SettingsError> {
        let current = self.current();
        self.store.ensure(&current).await?;
        let key = generate_admin_api_key();
        self.store
            .set_admin_api_key_hash(&hash_credential(&key))
            .await?;
        Ok(key)
    }

    pub async fn delete_admin_api_key(&self) -> Result<(), SettingsError> {
        self.store.clear_admin_api_key_hash().await
    }

    pub async fn verify_admin_api_key(&self, key: &str) -> Result<bool, SettingsError> {
        if key.is_empty() {
            return Ok(false);
        }
        let stored = self.store.load_admin_api_key_hash().await?;
        let key_hash = hash_credential(key);
        Ok(stored
            .as_deref()
            .filter(|stored_hash| !stored_hash.is_empty())
            .is_some_and(|stored_hash| key_hash.as_bytes().ct_eq(stored_hash.as_bytes()).into()))
    }
}

fn validate_model_aliases(
    aliases: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, SettingsValidationError> {
    let mut normalized = BTreeMap::new();
    for (alias, target) in aliases {
        let alias = nonempty("modelAliases", &alias)?;
        let target = nonempty("modelAliases", &target)?;
        if alias == target {
            return Err(invalid_field(
                "modelAliases",
                "alias and target must differ",
            ));
        }
        normalized.insert(alias, target);
    }
    Ok(normalized)
}

fn validate_rotation_strategy(strategy: &str) -> Result<String, SettingsValidationError> {
    let strategy = nonempty("rotationStrategy", strategy)?;
    if ROTATION_STRATEGIES.contains(&strategy.as_str()) {
        Ok(strategy)
    } else {
        Err(invalid_field(
            "rotationStrategy",
            "must be one of smart, quota_reset_priority, round_robin, sticky",
        ))
    }
}

fn nonempty(field: &str, value: &str) -> Result<String, SettingsValidationError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err(invalid_field(field, "must not be empty"))
    } else {
        Ok(value)
    }
}

fn positive_u64(field: &str, value: u64) -> Result<u64, SettingsValidationError> {
    (value > 0)
        .then_some(value)
        .ok_or_else(|| invalid_field(field, "must be greater than 0"))
}

fn positive_u32(field: &str, value: u32) -> Result<u32, SettingsValidationError> {
    (value > 0)
        .then_some(value)
        .ok_or_else(|| invalid_field(field, "must be greater than 0"))
}

fn positive_usize(field: &str, value: usize) -> Result<usize, SettingsValidationError> {
    (value > 0)
        .then_some(value)
        .ok_or_else(|| invalid_field(field, "must be greater than 0"))
}

fn invalid_field(field: &str, message: impl Into<String>) -> SettingsValidationError {
    SettingsValidationError::InvalidField {
        field: field.to_string(),
        message: message.into(),
    }
}
