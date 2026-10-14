//! 双凭据会话的签发、摘要校验与短窗口重试，不持久化可直接使用的凭据

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use rand_core::{OsRng, RngCore as _};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

use super::DefaultAuthService;
use crate::{
    model::{
        AdminError, AdminErrorKind,
        auth::{AuthSession, SessionGrant, SessionRefreshReplay, SessionRenewal, SessionSubject},
    },
    use_case::map_store_error,
};

const REPLAY_WINDOW: Duration = Duration::seconds(30);

pub(super) struct Credential<'a> {
    pub id: &'a str,
    pub secret: &'a str,
    pub refresh: bool,
}

impl<'a> Credential<'a> {
    pub fn parse(value: &'a str) -> Option<Self> {
        let mut parts = value.split('.');
        let refresh = match parts.next()? {
            "a2" => false,
            "r2" => true,
            _ => return None,
        };
        let id = parts.next()?;
        let secret = parts.next()?;
        if parts.next().is_some() || !valid_secret(id) || !valid_secret(secret) {
            return None;
        }
        Some(Self {
            id,
            secret,
            refresh,
        })
    }

    pub fn digest(&self) -> [u8; 32] {
        digest(self.secret)
    }

    pub fn owns(&self, renewal: &SessionRenewal) -> bool {
        if self.refresh {
            matches_digest(&renewal.refresh_digest, &self.digest())
                || renewal
                    .previous
                    .as_ref()
                    .is_some_and(|previous| matches_digest(&previous.digest, &self.digest()))
        } else {
            matches_digest(&renewal.access_digest, &self.digest())
        }
    }
}

fn valid_secret(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 32)
}

pub(super) fn storage_id(value: &str) -> &str {
    Credential::parse(value).map_or(value, |credential| credential.id)
}

fn matches_digest(left: &[u8; 32], right: &[u8; 32]) -> bool {
    bool::from(left.ct_eq(right))
}

fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn random_bytes() -> [u8; 32] {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

// 使用标准 HKDF 的独立用途标签；只有盐和摘要留在 Store，重试仍须持有原始凭据
fn derive(secret: &str, salt: Option<&[u8]>, purpose: &[u8]) -> Result<String, AdminError> {
    let mut output = [0; 32];
    hkdf::Hkdf::<Sha256>::new(salt, secret.as_bytes())
        .expand(purpose, &mut output)
        .map_err(|_| AdminError::internal("会话凭据派生失败"))?;
    Ok(URL_SAFE_NO_PAD.encode(output))
}

fn access_secret(refresh: &str) -> Result<String, AdminError> {
    derive(refresh, None, b"cpr-session-v2/access")
}

pub(super) fn issue(
    subject: SessionSubject,
    access_expiry: DateTime<Utc>,
    refresh_expiry: DateTime<Utc>,
    absolute: DateTime<Utc>,
) -> Result<SessionGrant, AdminError> {
    let id = URL_SAFE_NO_PAD.encode(random_bytes());
    let secret = URL_SAFE_NO_PAD.encode(random_bytes());
    let session = AuthSession {
        subject,
        expires_at: access_expiry,
        absolute_expires_at: Some(absolute),
        renewal: Some(SessionRenewal {
            expires_at: refresh_expiry,
            access_digest: digest(&access_secret(&secret)?),
            refresh_digest: digest(&secret),
            previous: None,
        }),
    };
    grant(&id, &secret, session)
}

fn grant(id: &str, refresh: &str, session: AuthSession) -> Result<SessionGrant, AdminError> {
    Ok(SessionGrant {
        session_id: format!("a2.{id}.{}", access_secret(refresh)?),
        refresh_token: Some(format!("r2.{id}.{refresh}")),
        session,
    })
}

impl DefaultAuthService {
    pub(super) async fn refresh(&self, raw: &str) -> Result<Option<SessionGrant>, AdminError> {
        let Some(credential) = Credential::parse(raw).filter(|value| value.refresh) else {
            return Ok(None);
        };
        let mut current = self.load_live_session(credential.id).await?;
        // CAS 冲突只重新解释同一请求的凭据，不对并发退出进行补建
        for _ in 0..3 {
            let Some(session) = current else {
                return Ok(None);
            };
            let Some(renewal) = &session.renewal else {
                return Ok(None);
            };
            let now = Utc::now();
            if session.retained_until() <= now
                || session.absolute_expires_at.is_none_or(|limit| limit <= now)
            {
                return Ok(None);
            }
            if matches_digest(&renewal.refresh_digest, &credential.digest()) {
                // 其他标签页已经刷新或客户端重试时复用当前代，避免 Cookie 响应相互覆盖
                if session.expires_at > now {
                    return grant(credential.id, credential.secret, session).map(Some);
                }
                let absolute = session
                    .absolute_expires_at
                    .ok_or_else(|| AdminError::internal("缺少会话最长有效期"))?;
                let salt = random_bytes();
                let next_secret =
                    derive(credential.secret, Some(&salt), b"cpr-session-v2/refresh")?;
                let refresh_expiry = (now + self.admin_session_ttl).min(absolute);
                let access_expiry = (now + self.admin_access_ttl).min(refresh_expiry);
                let replacement = AuthSession {
                    expires_at: access_expiry,
                    renewal: Some(SessionRenewal {
                        expires_at: refresh_expiry,
                        access_digest: digest(&access_secret(&next_secret)?),
                        refresh_digest: digest(&next_secret),
                        previous: Some(SessionRefreshReplay {
                            digest: credential.digest(),
                            salt,
                            expires_at: (now + REPLAY_WINDOW).min(access_expiry),
                        }),
                    }),
                    ..session.clone()
                };
                current = self
                    .store
                    .compare_exchange_session(credential.id, &session, Some(&replacement))
                    .await
                    .map_err(|error| map_store_error(error, "authentication session rotation"))?;
                if current.as_ref() == Some(&replacement) {
                    return grant(credential.id, &next_secret, replacement).map(Some);
                }
            } else if let Some(previous) = renewal
                .previous
                .as_ref()
                .filter(|previous| matches_digest(&previous.digest, &credential.digest()))
            {
                if previous.expires_at > now {
                    let secret = derive(
                        credential.secret,
                        Some(&previous.salt),
                        b"cpr-session-v2/refresh",
                    )?;
                    return grant(credential.id, &secret, session).map(Some);
                }
                // 已知上一代在重试窗口外再次使用，原子撤销整个会话
                current = self
                    .store
                    .compare_exchange_session(credential.id, &session, None)
                    .await
                    .map_err(|error| map_store_error(error, "authentication session revocation"))?;
            } else {
                return Ok(None);
            }
        }
        Err(AdminError::new(
            AdminErrorKind::Unavailable,
            "会话正在变更，请重试",
        ))
    }
}
