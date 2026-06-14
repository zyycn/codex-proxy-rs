use chrono::{DateTime, Utc};
use secrecy::SecretString;

use super::{
    status_to_db, AccountClaimsUpdate, AccountRepository, AccountRepositoryResult, TokenUpdate,
};

const UPDATE_TOKENS_WITH_REFRESH_SQL: &str = r"
update accounts
set
  access_token_cipher = ?,
  refresh_token_cipher = ?,
  access_token_expires_at = ?,
  status = case
    when status in ('disabled', 'banned') then status
    else 'active'
  end,
  updated_at = ?
where id = ?";

const UPDATE_TOKENS_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  access_token_cipher = ?,
  access_token_expires_at = ?,
  status = case
    when status in ('disabled', 'banned') then status
    else 'active'
  end,
  updated_at = ?
where id = ?";

const UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  account_id = ?,
  user_id = ?,
  plan_type = ?,
  access_token_cipher = ?,
  refresh_token_cipher = ?,
  access_token_expires_at = ?,
  status = ?,
  updated_at = ?
where id = ?";

const UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL: &str = r"
update accounts
set
  email = ?,
  account_id = ?,
  user_id = ?,
  plan_type = ?,
  access_token_cipher = ?,
  access_token_expires_at = ?,
  status = ?,
  updated_at = ?
where id = ?";

#[derive(Debug)]
struct TokenWrite {
    access_token_cipher: String,
    refresh_token_cipher: Option<String>,
    access_token_expires_at: Option<String>,
    updated_at: String,
}

impl AccountRepository {
    fn prepare_token_write(
        &self,
        access_token: &SecretString,
        refresh_token: Option<&SecretString>,
        access_token_expires_at: Option<DateTime<Utc>>,
    ) -> AccountRepositoryResult<TokenWrite> {
        Ok(TokenWrite {
            access_token_cipher: self.secret_box.encrypt(access_token)?,
            refresh_token_cipher: refresh_token
                .map(|token| self.secret_box.encrypt(token))
                .transpose()?,
            access_token_expires_at: access_token_expires_at.map(|value| value.to_rfc3339()),
            updated_at: Utc::now().to_rfc3339(),
        })
    }

    pub async fn update_tokens(
        &self,
        id: &str,
        update: TokenUpdate,
    ) -> AccountRepositoryResult<bool> {
        let TokenWrite {
            access_token_cipher,
            refresh_token_cipher,
            access_token_expires_at,
            updated_at,
        } = self.prepare_token_write(
            &update.access_token,
            update.refresh_token.as_ref(),
            update.access_token_expires_at,
        )?;

        let result = if let Some(refresh_token_cipher) = refresh_token_cipher {
            sqlx::query(UPDATE_TOKENS_WITH_REFRESH_SQL)
                .bind(access_token_cipher)
                .bind(refresh_token_cipher)
                .bind(access_token_expires_at)
                .bind(updated_at)
                .bind(id)
                .execute(&self.pool)
                .await?
        } else {
            // 刷新接口可能不返回新的 refresh_token；此时必须保留旧 RT，避免账号失去后续刷新能力。
            sqlx::query(UPDATE_TOKENS_PRESERVING_REFRESH_SQL)
                .bind(access_token_cipher)
                .bind(access_token_expires_at)
                .bind(updated_at)
                .bind(id)
                .execute(&self.pool)
                .await?
        };
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_from_claims(
        &self,
        id: &str,
        update: AccountClaimsUpdate,
    ) -> AccountRepositoryResult<bool> {
        let TokenWrite {
            access_token_cipher,
            refresh_token_cipher,
            access_token_expires_at,
            updated_at,
        } = self.prepare_token_write(
            &update.access_token,
            update.refresh_token.as_ref(),
            update.access_token_expires_at,
        )?;
        let status = status_to_db(update.status);

        let result = if let Some(refresh_token_cipher) = refresh_token_cipher {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL)
                .bind(update.email)
                .bind(update.account_id)
                .bind(update.user_id)
                .bind(update.plan_type)
                .bind(access_token_cipher)
                .bind(refresh_token_cipher)
                .bind(access_token_expires_at)
                .bind(status)
                .bind(updated_at)
                .bind(id)
                .execute(&self.pool)
                .await?
        } else {
            // OpenAI 刷新/导入未给新 RT 时保留原值，避免把可继续刷新的账号写坏。
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL)
                .bind(update.email)
                .bind(update.account_id)
                .bind(update.user_id)
                .bind(update.plan_type)
                .bind(access_token_cipher)
                .bind(access_token_expires_at)
                .bind(status)
                .bind(updated_at)
                .bind(id)
                .execute(&self.pool)
                .await?
        };
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::platform::crypto::SecretBox;

    #[tokio::test]
    async fn prepare_token_write_should_encrypt_tokens_and_format_timestamps() {
        let repository = AccountRepository::new(
            SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            SecretBox::new([55u8; 32]),
        );
        let access_token = SecretString::new("access-secret".to_string().into());
        let refresh_token = SecretString::new("refresh-secret".to_string().into());
        let expires_at = super::super::parse_rfc3339("2026-06-14T00:00:00Z").unwrap();

        let write = repository
            .prepare_token_write(&access_token, Some(&refresh_token), Some(expires_at))
            .unwrap();

        assert!(write.access_token_cipher.starts_with("v1:"));
        assert!(!write.access_token_cipher.contains("access-secret"));
        assert!(write.refresh_token_cipher.unwrap().starts_with("v1:"));
        assert_eq!(
            write.access_token_expires_at.as_deref(),
            Some("2026-06-14T00:00:00+00:00")
        );
        assert!(super::super::parse_rfc3339(&write.updated_at).is_ok());
    }
}
