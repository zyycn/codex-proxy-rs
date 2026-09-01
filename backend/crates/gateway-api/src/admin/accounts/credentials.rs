//! Provider-owned 凭据请求、命令转换与敏感材料校验。

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AccountProvider {
    OpenAi,
    Xai,
}

impl AccountProvider {
    fn parse(value: &str) -> Result<Self, WireValidationError> {
        match value.trim() {
            "openai" => Ok(Self::OpenAi),
            "xai" => Ok(Self::Xai),
            _ => Err(WireValidationError::new("provider")),
        }
    }
}

/// Provider-owned 账号导入请求；公共 API 不解释 `data` 内部字段。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountImportRequest {
    pub provider: String,
    pub data: Value,
}

impl AccountImportRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        AccountProvider::parse(&self.provider)?;
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
    ) -> Result<(AccountProvider, ImportCredentials), WireValidationError> {
        self.validate()?;
        let provider = AccountProvider::parse(&self.provider)?;
        Ok((
            provider,
            ImportCredentials {
                context,
                document: provider_document(self.data, "data")?,
            },
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartAccountAuthorizationRequest {
    pub provider: String,
    pub name: String,
    pub account_id: Option<String>,
}

impl StartAccountAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        AccountProvider::parse(&self.provider)?;
        require_text(&self.name, MAX_NAME_BYTES, "name")?;
        if let Some(account_id) = self.account_id.as_deref() {
            require_account_id(account_id, "accountId")?;
        }
        Ok(())
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<(AccountProvider, StartAuthorization), WireValidationError> {
        self.validate()?;
        let provider = AccountProvider::parse(&self.provider)?;
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
            },
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteAccountAuthorizationRequest {
    pub provider: String,
    pub flow_id: String,
    pub callback_url: String,
}

impl CompleteAccountAuthorizationRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        let provider = AccountProvider::parse(&self.provider)?;
        match provider {
            AccountProvider::OpenAi => {
                if !URL_SAFE_NO_PAD
                    .decode(&self.flow_id)
                    .is_ok_and(|decoded| decoded.len() == 32)
                {
                    return Err(WireValidationError::new("flowId"));
                }
            }
            AccountProvider::Xai => require_wire_id(&self.flow_id, "flowId")?,
        }
        require_text(&self.callback_url, MAX_CALLBACK_URL_BYTES, "callbackUrl")
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<(AccountProvider, CompleteAuthorization), WireValidationError> {
        self.validate()?;
        let provider = AccountProvider::parse(&self.provider)?;
        Ok((
            provider,
            CompleteAuthorization {
                context,
                flow_id: self.flow_id,
                callback_url: self.callback_url,
            },
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateAccountRequest {
    pub account_id: String,
    pub enabled: bool,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub concurrency_limit: Option<u64>,
    pub weight: u64,
    pub group_ids: Vec<String>,
}

impl UpdateAccountRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")?;
        parse_concurrency_limit(self.concurrency_limit)?;
        parse_account_weight(self.weight)?;
        validate_wire_group_ids(&self.group_ids)?;
        Ok(())
    }

    pub(super) fn into_command(self) -> Result<UpdateAccount, WireValidationError> {
        self.validate()?;
        Ok(UpdateAccount {
            account_id: self.account_id,
            enabled: self.enabled,
            concurrency_limit: parse_concurrency_limit(self.concurrency_limit)?,
            weight: parse_account_weight(self.weight)?,
            group_ids: validate_wire_group_ids(&self.group_ids)?,
        })
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
        AccountProvider::parse(&self.provider)?;
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
    ) -> Result<(AccountProvider, CredentialDeletion), WireValidationError> {
        self.validate()?;
        let provider = AccountProvider::parse(&self.provider)?;
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotateAccountRequest {
    pub provider: String,
    pub account_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
}

impl RotateAccountRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if AccountProvider::parse(&self.provider)? != AccountProvider::OpenAi {
            return Err(WireValidationError::new("provider"));
        }
        require_account_id(&self.account_id, "accountId")?;
        validate_oauth_material(
            &self.access_token,
            self.refresh_token.as_deref(),
            self.id_token.as_deref(),
        )
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<RotateCredential, WireValidationError> {
        self.validate()?;
        let mut material = Map::new();
        material.insert("access_token".to_owned(), Value::String(self.access_token));
        material.insert(
            "refresh_token".to_owned(),
            self.refresh_token.map_or(Value::Null, Value::String),
        );
        material.insert(
            "id_token".to_owned(),
            self.id_token.map_or(Value::Null, Value::String),
        );
        Ok(RotateCredential {
            mutation: CredentialMutation {
                context,
                account_id: ProviderAccountId::new(self.account_id)
                    .map_err(|_| WireValidationError::new("accountId"))?,
            },
            provider_material: ProviderDocument::new(OpaqueProviderData::new(material)),
        })
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

fn validate_oauth_material(
    access_token: &str,
    refresh_token: Option<&str>,
    id_token: Option<&str>,
) -> Result<(), WireValidationError> {
    if access_token.len() > MAX_ACCESS_TOKEN_BYTES
        || !valid_visible_ascii(access_token)
        || !valid_compact_jwt_shape(access_token)
    {
        return Err(WireValidationError::new("accessToken"));
    }
    if refresh_token.is_some_and(|token| {
        token.len() > MAX_REFRESH_TOKEN_BYTES
            || !valid_visible_ascii(token)
            || token == access_token
    }) {
        return Err(WireValidationError::new("refreshToken"));
    }
    if id_token.is_some_and(|token| {
        token.len() > MAX_ID_TOKEN_BYTES
            || !valid_visible_ascii(token)
            || !valid_compact_jwt_shape(token)
    }) {
        return Err(WireValidationError::new("idToken"));
    }
    Ok(())
}

fn valid_compact_jwt_shape(value: &str) -> bool {
    let mut segments = value.split('.');
    matches!(
        (segments.next(), segments.next(), segments.next(), segments.next()),
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty()
    )
}

fn valid_visible_ascii(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

pub(super) fn provider_document_value(document: ProviderDocument) -> Value {
    Value::Object(document.into_provider_data().into_inner())
}
