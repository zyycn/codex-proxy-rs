//! Codex 本地会话锚点的稳定派生。
//!
//! 旧版 Redis 会话亲和与 WebSocket 复用以运行数据目录下的 `identity_hmac_secret`
//! 生成的 `lc_` 值为键。该密钥是持久运行数据，不是账号凭据；保留它可避免
//! 架构迁移后同一客户端会话全部换键。

use std::fs;
use std::io::Write as _;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use super::protocol::responses::CodexResponsesRequest;
use super::request::derive_conversation_anchor;

/// OpenAI Provider 的持久会话锚点密钥。
#[derive(Clone)]
pub(crate) struct CodexSessionIdentity {
    secret: [u8; 32],
}

impl CodexSessionIdentity {
    pub(crate) fn load_or_create(path: &Path) -> Result<Self, CodexSessionIdentityError> {
        match read_secret(path)? {
            Some(secret) => Ok(Self { secret }),
            None => Self::create(path),
        }
    }

    fn create(path: &Path) -> Result<Self, CodexSessionIdentityError> {
        let parent = path
            .parent()
            .ok_or(CodexSessionIdentityError::InvalidPath)?;
        fs::create_dir_all(parent).map_err(|_| CodexSessionIdentityError::Storage)?;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|_| CodexSessionIdentityError::Storage)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            options.mode(0o600);
        }
        match options.open(path) {
            Ok(mut file) => {
                file.write_all(hex::encode(secret).as_bytes())
                    .map_err(|_| CodexSessionIdentityError::Storage)?;
                file.sync_all()
                    .map_err(|_| CodexSessionIdentityError::Storage)?;
                Ok(Self { secret })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_secret(path)?
                .map(|secret| Self { secret })
                .ok_or(CodexSessionIdentityError::InvalidSecret),
            Err(_) => Err(CodexSessionIdentityError::Storage),
        }
    }

    /// 在账号选择前固定一次旧版 `lc_` 会话键。
    pub(crate) fn prepare_local_conversation(&self, request: &mut CodexResponsesRequest) {
        if request.local_conversation_id.is_some() {
            return;
        }
        let Some((_, anchor)) = derive_conversation_anchor(request) else {
            return;
        };
        request.local_conversation_id = Some(format!(
            "lc_{}",
            URL_SAFE_NO_PAD.encode(hmac_sha256(
                &self.secret,
                &[b"local-conversation", b"\0", b"\0", anchor.as_bytes()],
            ))
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CodexSessionIdentityError {
    #[error("Codex session identity path is invalid")]
    InvalidPath,
    #[error("Codex session identity secret is invalid")]
    InvalidSecret,
    #[error("Codex session identity storage is unavailable")]
    Storage,
}

fn read_secret(path: &Path) -> Result<Option<[u8; 32]>, CodexSessionIdentityError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CodexSessionIdentityError::Storage),
    };
    let bytes = hex::decode(raw.trim()).map_err(|_| CodexSessionIdentityError::InvalidSecret)?;
    bytes
        .try_into()
        .map(Some)
        .map_err(|_| CodexSessionIdentityError::InvalidSecret)
}

fn hmac_sha256(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;

    let mut key_block = [0_u8; BLOCK_BYTES];
    key_block[..key.len()].copy_from_slice(key);
    let mut inner_pad = key_block;
    let mut outer_pad = key_block;
    for byte in &mut inner_pad {
        *byte ^= 0x36;
    }
    for byte in &mut outer_pad {
        *byte ^= 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for part in parts {
        inner.update(part);
    }
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}
