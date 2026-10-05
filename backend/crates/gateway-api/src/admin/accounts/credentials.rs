//! Provider-owned 凭据请求、命令转换与敏感材料校验

use super::*;

fn validate_account_notes(notes: Option<&str>) -> Result<(), WireValidationError> {
    if notes.is_some_and(|notes| {
        notes.chars().count() > 500
            || notes
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    }) {
        return Err(WireValidationError::new("notes"));
    }
    Ok(())
}

fn parse_provider(value: &str) -> Result<ProviderKind, WireValidationError> {
    ProviderKind::new(value.trim().to_owned()).map_err(|_| WireValidationError::new("provider"))
}

/// 导入统一设置，复用编辑账号的备注、调度和分组约束
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportSettingsRequest {
    pub notes: Option<String>,
    pub enabled: bool,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub concurrency_limit: Option<u64>,
    pub weight: u64,
    pub model_access: Option<gateway_core::account::AccountModelAccess>,
    pub group_ids: Vec<String>,
}

impl AccountImportSettingsRequest {
    fn validate(&self) -> Result<(), WireValidationError> {
        validate_account_notes(self.notes.as_deref())?;
        parse_concurrency_limit(self.concurrency_limit)?;
        parse_account_weight(self.weight)?;
        validate_wire_group_ids(&self.group_ids)?;
        Ok(())
    }

    fn into_settings(
        self,
    ) -> Result<gateway_admin::model::accounts::AccountImportSettings, WireValidationError> {
        Ok(gateway_admin::model::accounts::AccountImportSettings {
            notes: self.notes,
            enabled: self.enabled,
            concurrency_limit: parse_concurrency_limit(self.concurrency_limit)?,
            weight: parse_account_weight(self.weight)?,
            model_access: self.model_access,
            group_ids: validate_wire_group_ids(&self.group_ids)?,
        })
    }
}

/// Provider-owned 账号导入请求；公共 API 不解释 `data` 内部字段
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportRequest {
    pub outbound_proxy_id: Option<String>,
    pub settings: Option<AccountImportSettingsRequest>,
    pub provider: String,
    pub data: Value,
}

impl AccountImportRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if let Some(id) = &self.outbound_proxy_id {
            require_wire_id(id, "outboundProxyId")?;
        }
        if let Some(settings) = &self.settings {
            settings.validate()?;
        }
        parse_provider(&self.provider)?;
        if !self.data.is_object()
            || serde_json::to_vec(&self.data)
                .map_or(true, |encoded| encoded.len() > MAX_IMPORT_DATA_BYTES)
        {
            return Err(WireValidationError::new("data"));
        }
        Ok(())
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<(ProviderKind, ImportCredentials), WireValidationError> {
        self.validate()?;
        let provider = parse_provider(&self.provider)?;
        Ok((
            provider,
            ImportCredentials {
                outbound_proxy_id: self.outbound_proxy_id,
                settings: self
                    .settings
                    .map(AccountImportSettingsRequest::into_settings)
                    .transpose()?,
                context,
                document: provider_document(self.data, "data")?,
            },
        ))
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartAccountAuthorizationRequest {
    pub outbound_proxy_id: Option<String>,
    pub provider: String,
    pub name: String,
    pub account_id: Option<String>,
    pub outbound_proxy_url: Option<super::wire::AccountProxyUpdate>,
}

impl StartAccountAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        parse_provider(&self.provider)?;
        require_text(&self.name, MAX_NAME_BYTES, "name")?;
        if let Some(account_id) = self.account_id.as_deref() {
            require_account_id(account_id, "accountId")?;
        }
        Ok(())
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<(ProviderKind, StartAuthorization), WireValidationError> {
        self.validate()?;
        let provider = parse_provider(&self.provider)?;
        let reauthorization = self
            .account_id
            .map(ProviderAccountId::new)
            .transpose()
            .map_err(|_| WireValidationError::new("accountId"))?;
        Ok((
            provider,
            StartAuthorization {
                context,
                name: self.name,
                reauthorization,
                outbound_proxy: super::wire::proxy_selection(
                    self.outbound_proxy_id,
                    self.outbound_proxy_url,
                )?,
            },
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteAccountAuthorizationRequest {
    pub settings: Option<AccountImportSettingsRequest>,
    pub provider: String,
    pub flow_id: String,
    pub callback_url: String,
}

impl CompleteAccountAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if let Some(settings) = &self.settings {
            settings.validate()?;
        }
        let provider = parse_provider(&self.provider)?;
        validate_authorization_flow(&provider, &self.flow_id)?;
        require_text(&self.callback_url, MAX_CALLBACK_URL_BYTES, "callbackUrl")
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<(ProviderKind, CompleteAuthorization), WireValidationError> {
        self.validate()?;
        let provider = parse_provider(&self.provider)?;
        Ok((
            provider,
            CompleteAuthorization {
                settings: self
                    .settings
                    .map(AccountImportSettingsRequest::into_settings)
                    .transpose()?,
                context,
                flow_id: self.flow_id,
                callback_url: self.callback_url,
            },
        ))
    }
}

fn validate_authorization_flow(
    provider: &ProviderKind,
    flow_id: &str,
) -> Result<(), WireValidationError> {
    if provider.as_str() == "openai" {
        if !URL_SAFE_NO_PAD
            .decode(flow_id)
            .is_ok_and(|decoded| decoded.len() == 32)
        {
            return Err(WireValidationError::new("flowId"));
        }
        Ok(())
    } else {
        require_wire_id(flow_id, "flowId")
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateAccountRequest {
    pub connection: Option<AccountConnectionUpdateRequest>,
    pub outbound_proxy_id: Option<String>,
    pub outbound_proxy_url: Option<super::wire::AccountProxyUpdate>,
    pub account_id: String,
    pub notes: Option<String>,
    pub enabled: bool,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub concurrency_limit: Option<u64>,
    pub weight: u64,
    pub model_access: Option<gateway_core::account::AccountModelAccess>,
    pub group_ids: Vec<String>,
}

impl UpdateAccountRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")?;
        if let Some(connection) = &self.connection {
            connection.validate()?;
        }
        validate_account_notes(self.notes.as_deref())?;
        parse_concurrency_limit(self.concurrency_limit)?;
        parse_account_weight(self.weight)?;
        validate_wire_group_ids(&self.group_ids)?;
        Ok(())
    }

    pub(super) fn into_command(
        self,
    ) -> Result<(UpdateAccount, Option<ProviderDocument>), WireValidationError> {
        self.validate()?;
        let connection = self
            .connection
            .map(AccountConnectionUpdateRequest::into_document);
        let settings = UpdateAccount {
            outbound_proxy: super::wire::proxy_selection(
                self.outbound_proxy_id,
                self.outbound_proxy_url,
            )?,
            account_id: self.account_id,
            notes: self.notes,
            enabled: self.enabled,
            concurrency_limit: parse_concurrency_limit(self.concurrency_limit)?,
            weight: parse_account_weight(self.weight)?,
            model_access: self.model_access,
            group_ids: validate_wire_group_ids(&self.group_ids)?,
        };
        Ok((settings, connection))
    }
}

/// 编辑 OpenAI 账号的连接设置；OAuth 仅接受传输方式
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountConnectionUpdateRequest {
    pub base_url: Option<String>,
    pub transport: String,
    pub api_key: Option<String>,
}

impl AccountConnectionUpdateRequest {
    fn validate(&self) -> Result<(), WireValidationError> {
        if let Some(base_url) = &self.base_url {
            require_text(base_url, 2048, "connection.baseUrl")?;
        }
        if !matches!(self.transport.as_str(), "http" | "prefer_websocket") {
            return Err(WireValidationError::new("connection.transport"));
        }
        if self.api_key.as_ref().is_some_and(|key| {
            key.is_empty()
                || key.len() > 16 * 1024
                || !key.bytes().all(|byte| byte.is_ascii_graphic())
        }) {
            return Err(WireValidationError::new("connection.apiKey"));
        }
        Ok(())
    }

    fn into_document(self) -> ProviderDocument {
        let mut material =
            Map::from_iter([("transport".to_owned(), Value::String(self.transport))]);
        if let Some(base_url) = self.base_url {
            material.insert("base_url".to_owned(), Value::String(base_url));
        }
        if let Some(key) = self.api_key {
            material.insert("api_key".to_owned(), Value::String(key));
        }
        ProviderDocument::new(OpaqueProviderData::new(material))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedAccountData {
    pub account_id: String,
    pub config_revision: u64,
}

impl From<AccountUpdateResult> for UpdatedAccountData {
    fn from(result: AccountUpdateResult) -> Self {
        Self {
            account_id: result.account_id.to_string(),
            config_revision: result.config_revision.get(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountDeletionRequest {
    pub provider: String,
    pub account_ids: Vec<String>,
}

impl AccountDeletionRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        parse_provider(&self.provider)?;
        if self.account_ids.is_empty() || self.account_ids.len() > MAX_ACCOUNT_DELETE_BATCH {
            return Err(WireValidationError::new("accountIds"));
        }
        let mut unique = BTreeSet::new();
        for account_id in &self.account_ids {
            require_account_id(account_id, "accountIds")?;
            if !unique.insert(account_id.as_str()) {
                return Err(WireValidationError::new("accountIds"));
            }
        }
        Ok(())
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<(ProviderKind, CredentialDeletion), WireValidationError> {
        self.validate()?;
        let provider = parse_provider(&self.provider)?;
        Ok((
            provider,
            CredentialDeletion {
                context,
                account_ids: self
                    .account_ids
                    .into_iter()
                    .map(ProviderAccountId::new)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| WireValidationError::new("accountIds"))?,
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountImportData {
    pub imported_count: usize,
    pub account_ids: Vec<String>,
}

impl AccountImportData {
    pub fn from_result(result: CredentialImportResult) -> Self {
        let account_ids = result
            .credential_ids
            .into_iter()
            .map(|account_id| account_id.to_string())
            .collect::<Vec<_>>();
        Self {
            imported_count: account_ids.len(),
            account_ids,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAuthorizationData {
    pub flow_id: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

impl From<AuthorizationStarted> for AccountAuthorizationData {
    fn from(started: AuthorizationStarted) -> Self {
        Self {
            flow_id: started.flow_id,
            authorization_url: started.authorization_url,
            expires_at: started.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountMutationData {
    pub account_id: String,
}

impl From<CredentialMutationResult> for AccountMutationData {
    fn from(result: CredentialMutationResult) -> Self {
        Self {
            account_id: result.account_id.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDeletionData {
    pub deleted_count: usize,
    pub account_ids: Vec<String>,
}

impl From<CredentialDeletionResult> for AccountDeletionData {
    fn from(result: CredentialDeletionResult) -> Self {
        let account_ids = result
            .account_ids
            .into_iter()
            .map(|account_id| account_id.to_string())
            .collect::<Vec<_>>();
        Self {
            deleted_count: account_ids.len(),
            account_ids,
        }
    }
}

pub(super) fn require_account_id(
    value: &str,
    field: &'static str,
) -> Result<(), WireValidationError> {
    if value.trim().is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
        || !value.starts_with("acct_")
    {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

pub(super) fn parse_concurrency_limit(
    value: Option<u64>,
) -> Result<Option<AccountConcurrencyLimit>, WireValidationError> {
    value
        .map(|value| {
            u32::try_from(value)
                .ok()
                .and_then(AccountConcurrencyLimit::new)
                .ok_or_else(|| WireValidationError::new("concurrencyLimit"))
        })
        .transpose()
}

pub(super) fn deserialize_required_nullable<'de, D>(
    deserializer: D,
) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer)
}

pub(super) fn parse_account_weight(value: u64) -> Result<AccountWeight, WireValidationError> {
    u16::try_from(value)
        .ok()
        .and_then(AccountWeight::new)
        .ok_or_else(|| WireValidationError::new("weight"))
}

pub(super) fn validate_wire_group_ids(
    values: &[String],
) -> Result<Vec<AccountGroupId>, WireValidationError> {
    if values.len() > MAX_ACCOUNT_GROUP_BATCH
        || values.iter().collect::<BTreeSet<_>>().len() != values.len()
    {
        return Err(WireValidationError::new("groupIds"));
    }
    values
        .iter()
        .cloned()
        .map(|value| AccountGroupId::new(value).map_err(|_| WireValidationError::new("groupIds")))
        .collect()
}

fn provider_document(
    value: Value,
    field: &'static str,
) -> Result<ProviderDocument, WireValidationError> {
    match value {
        Value::Object(document) => Ok(ProviderDocument::new(OpaqueProviderData::new(document))),
        _ => Err(WireValidationError::new(field)),
    }
}

fn require_wire_id(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    require_text(value, MAX_ID_BYTES, field)?;
    if value.starts_with("__")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

fn require_text(
    value: &str,
    max_bytes: usize,
    field: &'static str,
) -> Result<(), WireValidationError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(WireValidationError::new(field));
    }
    Ok(())
}

pub(super) fn provider_document_value(document: ProviderDocument) -> Value {
    Value::Object(document.into_provider_data().into_inner())
}
