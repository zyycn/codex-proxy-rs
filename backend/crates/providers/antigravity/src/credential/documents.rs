//! CPR 原生账号文件包含可续期凭据与已存观测，导入重新验证身份与权限

use super::{
    CredentialService, Material,
    oauth::{error, valid_token},
};
use chrono::{DateTime, Utc};
use gateway_admin::{
    model::provider_credentials::{
        PrepareCredentialImport, PrepareCredentialRotation, PreparedCredentialImport,
        PreparedCredentialRotation, ProviderDocument, ProviderExport,
        ProviderExportCredentialInput,
    },
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind as Kind},
};
use gateway_core::account::{OpaqueProviderData, ProviderAccountId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AccountFile {
    provider: String,
    version: u32,
    accounts: Vec<AccountEntry>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccountEntry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    label: String,
    refresh_token: String,
    // 以下为导出时的已有观测，导入仍以 Google 刷新及身份查询的结果为准
    #[serde(skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    access_token_expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scopes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
}

impl CredentialService {
    pub(crate) async fn prepare_import(
        &self,
        input: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        let file: AccountFile = serde_json::from_value(Value::Object(
            input.document.into_provider_data().into_inner(),
        ))
        .map_err(|_| error(Kind::Invalid))?;
        if file.provider != self.kind.as_str() || file.version != 1 || file.accounts.is_empty() {
            return Err(error(Kind::Invalid));
        }
        let mut tokens = HashSet::new();
        // 全部本地格式检查先于任何可能轮换 RT 的上游请求
        for entry in &file.accounts {
            if entry.label.chars().any(char::is_control)
                || !valid_token(&entry.refresh_token)
                || !tokens.insert(entry.refresh_token.as_str())
            {
                return Err(error(Kind::Invalid));
            }
        }
        let mut credentials = Vec::with_capacity(file.accounts.len());
        let mut subjects = HashSet::new();
        let profile = self.request_profile().await?;
        for entry in file.accounts {
            let grant = self
                .oauth
                .verify_import(
                    &entry.refresh_token,
                    input.default_outbound_proxy.as_ref(),
                    &profile,
                )
                .await?;
            if !subjects.insert(grant.material.subject.clone()) {
                return Err(
                    error(Kind::Conflict).with_public_message("导入文件包含重复的 Google 账号")
                );
            }
            credentials.push(self.prepare_create(
                entry.label,
                grant,
                input.default_outbound_proxy.clone(),
            )?);
        }
        Ok(PreparedCredentialImport {
            provider_kind: self.kind.clone(),
            credentials,
        })
    }

    pub(crate) async fn prepare_rotation(
        &self,
        input: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        if input.account.provider_kind != self.kind || input.account.authentication_kind != "oauth"
        {
            return Err(error(Kind::Invalid));
        }
        let account_id =
            ProviderAccountId::new(input.account.id).map_err(|_| error(Kind::Invalid))?;
        let current = self.current(&account_id).await?;
        if current.account.revision().get() != input.account.credential_revision.get() {
            return Err(error(Kind::Conflict));
        }
        let entry: AccountEntry = serde_json::from_value(Value::Object(
            input.provider_material.into_provider_data().into_inner(),
        ))
        .map_err(|_| error(Kind::Invalid))?;
        let guards = self.lock_rotation(&current.account).await?;
        let profile = self.request_profile().await?;
        let grant = self
            .oauth
            .verify_import(
                &entry.refresh_token,
                current.account.outbound_proxy(),
                &profile,
            )
            .await?;
        // 同一 subject 的更新保持本地名称；另一账号只能作为新账号导入
        self.rotation(&current.account, grant, guards)
    }

    pub(crate) fn export_credentials(
        &self,
        credentials: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError> {
        let mut account_ids = Vec::with_capacity(credentials.len());
        let mut accounts = Vec::with_capacity(credentials.len());
        for input in credentials {
            if input.account.provider_kind != self.kind
                || input.account.authentication_kind != "oauth"
            {
                return Err(error(Kind::Invalid));
            }
            let material: Material = serde_json::from_value(Value::Object(
                input.provider_material.into_provider_data().into_inner(),
            ))
            .map_err(|_| error(Kind::Invalid))?;
            if material.registration != self.oauth.registration()
                || input.account.upstream_user_id.as_deref() != Some(material.subject.as_str())
                || !valid_token(&material.refresh_token)
            {
                return Err(error(Kind::Invalid));
            }
            account_ids
                .push(ProviderAccountId::new(input.account.id).map_err(|_| error(Kind::Invalid))?);
            accounts.push(AccountEntry {
                label: input.account.name,
                refresh_token: material.refresh_token,
                access_token: Some(material.access_token),
                access_token_expires_at: input.account.access_token_expires_at,
                email: input.account.email,
                user_id: Some(material.subject),
                scopes: Some(material.scopes),
                plan_type: input.account.plan_type,
                created_at: Some(input.account.created_at),
                updated_at: Some(input.account.updated_at),
            });
        }
        let Value::Object(document) = serde_json::to_value(AccountFile {
            provider: self.kind.as_str().to_owned(),
            version: 1,
            accounts,
        })
        .map_err(|_| error(Kind::Internal))?
        else {
            return Err(error(Kind::Internal));
        };
        Ok(ProviderExport {
            provider_kind: self.kind.clone(),
            account_ids,
            document: ProviderDocument::new(OpaqueProviderData::new(document)),
        })
    }
}
