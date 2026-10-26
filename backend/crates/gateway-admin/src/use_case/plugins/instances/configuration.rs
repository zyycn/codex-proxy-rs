//! 插件配置的输入约束、创建幂等比较与候选快照变换

use std::collections::BTreeSet;

use gateway_core::{policy::ClientApiKeyId, routing::AccountGroupId};
use secrecy::ExposeSecret;

use crate::model::{
    AdminError, Revision,
    plugins::instances::{
        ConfigurePluginInstance, PluginInstance, PluginInstanceReplacement, PluginInstanceSnapshot,
    },
};

pub(super) fn apply_replacements(
    snapshot: &mut PluginInstanceSnapshot,
    replacements: &[PluginInstanceReplacement],
) {
    for instance in &mut snapshot.instances {
        if replacements.iter().any(|item| item.id == instance.id) {
            instance.enabled = false;
            instance.revision = snapshot.config_revision;
        }
    }
}

pub(super) fn same_creation_request(
    existing: &PluginInstance,
    input: &ConfigurePluginInstance,
) -> bool {
    let same_secrets = input
        .secrets
        .as_ref()
        .map_or(existing.secrets.is_empty(), |secrets| {
            secrets.len() == existing.secrets.len()
                && secrets.iter().all(|(key, value)| {
                    existing
                        .secrets
                        .get(key)
                        .is_some_and(|current| current.expose_secret() == value.expose_secret())
                })
        });
    existing.name == input.name.trim()
        && existing.artifact_sha256 == input.artifact_sha256
        && existing.enabled == input.enabled
        && existing.configuration == input.configuration
        && existing.bindings == input.bindings
        && same_secrets
}

pub(super) fn validate_input(instance: &PluginInstance) -> Result<(), AdminError> {
    if instance.name.is_empty()
        || instance.name.len() > 128
        || instance.name.chars().any(char::is_control)
        || instance.artifact_sha256.len() != 64
        || !instance
            .artifact_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !instance.configuration.is_object()
        || serde_json::to_vec(&instance.configuration).map_or(true, |bytes| bytes.len() > 48 * 1024)
        || instance.secrets.len() > 64
        || instance.bindings.len() > 64
    {
        return Err(AdminError::invalid("插件实例配置不合法或超过大小限制"));
    }
    use secrecy::ExposeSecret as _;
    if instance
        .secrets
        .iter()
        .any(|(key, value)| key.len() > 64 || value.expose_secret().len() > 8192)
        || instance
            .secrets
            .values()
            .map(|value| value.expose_secret().len())
            .sum::<usize>()
            > 48 * 1024
    {
        return Err(AdminError::invalid("插件敏感配置超过大小限制"));
    }
    if instance.bindings.iter().any(|binding| {
        binding.contribution.is_empty()
            || binding.contribution.len() > 128
            || !binding
                .contribution
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            || binding.client_key_ids.len() > 256
            || binding.account_group_ids.len() > 256
            || binding.client_key_ids.iter().collect::<BTreeSet<_>>().len()
                != binding.client_key_ids.len()
            || binding
                .account_group_ids
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != binding.account_group_ids.len()
            || binding
                .client_key_ids
                .iter()
                .any(|id| ClientApiKeyId::new(id.clone()).is_err())
            || binding
                .account_group_ids
                .iter()
                .any(|id| AccountGroupId::new(id.clone()).is_err())
            || binding.identity_bindings.len() > 256
            || binding
                .identity_bindings
                .iter()
                .map(|identity| &identity.principal)
                .collect::<BTreeSet<_>>()
                .len()
                != binding.identity_bindings.len()
            || binding.identity_bindings.iter().any(|identity| {
                identity.principal.trim() != identity.principal
                    || identity.principal.is_empty()
                    || identity.principal.len() > 256
                    || identity.principal.chars().any(char::is_control)
                    || ClientApiKeyId::new(identity.client_key_id.clone()).is_err()
            })
    }) {
        return Err(AdminError::invalid("插件绑定的范围或入口认证身份不合法"));
    }
    Ok(())
}

pub(super) fn next_revision(revision: Revision) -> Result<Revision, AdminError> {
    Revision::new(
        revision
            .get()
            .checked_add(1)
            .ok_or_else(|| AdminError::internal("配置版本已耗尽"))?,
    )
    .map_err(|_| AdminError::internal("配置版本不合法"))
}
