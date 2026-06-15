use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    codex::accounts::{
        model::{Account, AccountStatus},
        repository::{AccountClaimsUpdate, AccountRepository, NewAccount, StoredAccount},
    },
    codex::gateway::oauth::RefreshFailure,
};

use super::{
    pool_sync::pool_account_from_stored, AccountImportCounts, AccountImportEntry, AccountService,
    StoreImportAccountError, ValidatedAccountImportError,
};

enum StoredImportAccount {
    Imported,
    Skipped,
}

impl AccountService {
    pub async fn import_validated(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<StoredAccount, ValidatedAccountImportError> {
        let repo = self
            .repository
            .as_ref()
            .ok_or(ValidatedAccountImportError::RepositoryUnavailable)?;
        let (access_token, refresh_token_update, new_account_refresh_token) = match (
            empty_to_none(token.map(normalize_bearer_token)),
            empty_to_none(refresh_token),
        ) {
            (Some(token), refresh_token) => (token, refresh_token.clone(), refresh_token),
            (None, Some(refresh_token)) => {
                let Some(refresher) = self.token_refresher.as_ref() else {
                    return Err(ValidatedAccountImportError::TokenRefresherUnavailable);
                };
                let tokens = match refresher.refresh(&refresh_token).await {
                    Ok(tokens) => tokens,
                    Err(RefreshFailure::Transport) => {
                        return Err(ValidatedAccountImportError::RefreshTransport);
                    }
                    Err(_) => return Err(ValidatedAccountImportError::RefreshRejected),
                };
                let access_token = normalize_bearer_token(tokens.access_token);
                let rotated_refresh_token = empty_to_none(tokens.refresh_token);
                let new_account_refresh_token =
                    rotated_refresh_token.clone().or(Some(refresh_token));
                (
                    access_token,
                    rotated_refresh_token,
                    new_account_refresh_token,
                )
            }
            (None, None) => return Err(ValidatedAccountImportError::TokenRequired),
        };

        let claims = manual_account_claims(&access_token, Utc::now())
            .map_err(ValidatedAccountImportError::InvalidToken)?;
        let existing = repo
            .find_by_chatgpt_identity(&claims.account_id, claims.user_id.as_deref())
            .await
            .map_err(|_| ValidatedAccountImportError::Inspect)?;

        let account_id = if let Some(existing) = existing {
            let updated = repo
                .update_from_claims(
                    &existing.id,
                    AccountClaimsUpdate {
                        email: claims.email.clone(),
                        account_id: Some(claims.account_id.clone()),
                        user_id: claims.user_id.clone(),
                        plan_type: claims.plan_type.clone(),
                        access_token: SecretString::new(access_token.into()),
                        refresh_token: refresh_token_update
                            .map(|token| SecretString::new(token.into())),
                        access_token_expires_at: Some(claims.expires_at),
                        status: AccountStatus::Active,
                    },
                )
                .await
                .map_err(|_| ValidatedAccountImportError::Update)?;
            if !updated {
                return Err(ValidatedAccountImportError::NotFound);
            }
            existing.id
        } else {
            let id = normalized_account_id(None);
            let account = NewAccount {
                id: id.clone(),
                email: claims.email.clone(),
                account_id: Some(claims.account_id.clone()),
                user_id: claims.user_id.clone(),
                label: None,
                plan_type: claims.plan_type.clone(),
                access_token: SecretString::new(access_token.into()),
                refresh_token: new_account_refresh_token
                    .map(|token| SecretString::new(token.into())),
                access_token_expires_at: Some(claims.expires_at),
                status: AccountStatus::Active,
            };
            repo.insert(account)
                .await
                .map_err(|_| ValidatedAccountImportError::Insert)?;
            id
        };

        let stored = repo
            .get(&account_id)
            .await
            .map_err(|_| ValidatedAccountImportError::Load)?
            .ok_or(ValidatedAccountImportError::NotFound)?;
        self.account_pool
            .lock()
            .await
            .insert(pool_account_from_stored(stored.clone()));
        Ok(stored)
    }

    pub async fn import_entries(
        &self,
        entries: Vec<AccountImportEntry>,
    ) -> Result<AccountImportCounts, StoreImportAccountError> {
        let repo = self
            .repository
            .as_ref()
            .ok_or(StoreImportAccountError::RepositoryUnavailable)?;
        let mut imported = 0u32;
        let mut skipped = 0u32;
        for entry in entries {
            match self.store_import_entry(repo, entry).await? {
                StoredImportAccount::Imported => imported += 1,
                StoredImportAccount::Skipped => skipped += 1,
            }
        }
        Ok(AccountImportCounts { imported, skipped })
    }

    async fn store_import_entry(
        &self,
        repo: &AccountRepository,
        entry: AccountImportEntry,
    ) -> Result<StoredImportAccount, StoreImportAccountError> {
        let access_token = entry.token.as_deref().unwrap_or_default().trim();
        if access_token.is_empty() {
            return Ok(StoredImportAccount::Skipped);
        }
        if entry
            .label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(StoreImportAccountError::Invalid(
                "Account label must be 64 characters or fewer".to_string(),
            ));
        }

        let id = normalized_account_id(entry.id);
        match repo.exists(&id).await {
            Ok(true) => return Ok(StoredImportAccount::Skipped),
            Ok(false) => {}
            Err(_) => return Err(StoreImportAccountError::Inspect),
        }

        let status = parse_import_status(entry.status.as_deref())
            .map_err(StoreImportAccountError::Invalid)?;
        let email = empty_to_none(entry.email);
        let account_id = empty_to_none(entry.account_id);
        let user_id = empty_to_none(entry.user_id);
        let label = empty_to_none(entry.label);
        let plan_type = empty_to_none(entry.plan_type);
        let refresh_token = empty_to_none(entry.refresh_token);
        let access_token_expires_at = entry
            .access_token_expires_at
            .as_deref()
            .map(parse_import_datetime)
            .transpose()
            .map_err(StoreImportAccountError::Invalid)?;
        let access_token = access_token.to_string();
        let now = Utc::now().to_rfc3339();
        let pool_account = Account {
            id: id.clone(),
            email: email.clone(),
            account_id: account_id.clone(),
            user_id: user_id.clone(),
            label: label.clone(),
            plan_type: plan_type.clone(),
            access_token: access_token.clone(),
            refresh_token: refresh_token.clone(),
            access_token_expires_at,
            status,
            quota_limit_reached: false,
            quota_verify_required: false,
            quota_cooldown_until: None,
            cloudflare_cooldown_until: None,
            request_count: 0,
            empty_response_count: 0,
            window_request_count: 0,
            window_input_tokens: 0,
            window_output_tokens: 0,
            window_cached_tokens: 0,
            window_started_at: None,
            window_reset_at: None,
            limit_window_seconds: None,
            added_at: now,
            last_used_at: None,
        };
        let account = NewAccount {
            id,
            email,
            account_id,
            user_id,
            label,
            plan_type,
            access_token: SecretString::new(access_token.into()),
            refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
            access_token_expires_at,
            status,
        };
        repo.insert(account)
            .await
            .map_err(|_| StoreImportAccountError::Insert)?;
        self.account_pool.lock().await.insert(pool_account);

        Ok(StoredImportAccount::Imported)
    }
}

#[derive(Debug, Clone)]
pub(super) struct ManualAccountClaims {
    pub account_id: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub expires_at: DateTime<Utc>,
}

pub(super) fn manual_account_claims(
    token: &str,
    now: DateTime<Utc>,
) -> Result<ManualAccountClaims, &'static str> {
    let payload = decode_jwt_payload(token).ok_or("Invalid JWT format")?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or("Token is expired")?;
    if now.timestamp() >= exp {
        return Err("Token is expired");
    }
    let expires_at = DateTime::<Utc>::from_timestamp(exp, 0).ok_or("Invalid JWT exp claim")?;
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .ok_or("Token missing chatgpt_account_id claim")?;
    let account_id =
        string_claim(auth, "chatgpt_account_id").ok_or("Token missing chatgpt_account_id claim")?;
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let user_id = string_claim(auth, "chatgpt_user_id")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_user_id")));
    let plan_type = string_claim(auth, "chatgpt_plan_type")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_plan_type")));
    let email = profile.and_then(|profile| string_claim(profile, "email"));

    Ok(ManualAccountClaims {
        account_id,
        user_id,
        email,
        plan_type,
        expires_at,
    })
}

pub(super) fn normalize_bearer_token(value: String) -> String {
    value
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| value.trim().strip_prefix("bearer "))
        .unwrap_or(value.trim())
        .trim()
        .to_string()
}

pub(super) fn empty_to_none(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_import_datetime(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| "Invalid accessTokenExpiresAt".to_string())
}

pub(super) fn normalized_account_id(id: Option<String>) -> String {
    id.and_then(|id| empty_to_none(Some(id)))
        .unwrap_or_else(|| format!("acct_{}", Uuid::new_v4().simple()))
}

pub(super) fn parse_import_status(status: Option<&str>) -> Result<AccountStatus, String> {
    let normalized = status.unwrap_or("active").trim().to_ascii_lowercase();
    match normalized.as_str() {
        "active" => Ok(AccountStatus::Active),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" => Ok(AccountStatus::QuotaExhausted),
        "refreshing" => Ok(AccountStatus::Refreshing),
        "disabled" => Ok(AccountStatus::Disabled),
        "banned" => Ok(AccountStatus::Banned),
        other => Err(format!("Unsupported account status: {other}")),
    }
}

fn decode_jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .as_object()
        .cloned()
}

fn string_claim(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
