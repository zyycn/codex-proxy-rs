use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

use crate::{OAuthError, SecretValue};

pub struct Pkce {
    verifier: SecretValue,
    challenge: String,
}

impl Pkce {
    pub(crate) fn generate() -> Result<Self, OAuthError> {
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|_| OAuthError::EntropyUnavailable)?;
        Ok(Self::from_verifier_bytes(&random))
    }

    fn from_verifier_bytes(bytes: &[u8]) -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        Self::from_verifier(verifier)
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier: SecretValue::new(verifier),
            challenge,
        }
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    pub(crate) fn into_verifier(self) -> SecretValue {
        self.verifier
    }
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pkce")
            .field("verifier", &"[REDACTED]")
            .field("challenge", &self.challenge)
            .finish()
    }
}
