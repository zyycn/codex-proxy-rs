use serde::Deserialize;
use url::Url;

use crate::{GrokOAuthConfig, OAuthError, OAuthOperation, ProtocolViolation};

pub(crate) const MAX_OAUTH_RESPONSE_BYTES: usize = 64 * 1024;

/// A discovery document whose issuer and endpoints have passed the official
/// Grok Build origin policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryDocument {
    issuer: Url,
    authorization_endpoint: Url,
    token_endpoint: Url,
    jwks_uri: Url,
    userinfo_endpoint: Url,
    signing_algorithms: Vec<String>,
}

impl DiscoveryDocument {
    pub fn parse(config: &GrokOAuthConfig, body: &[u8]) -> Result<Self, OAuthError> {
        if body.len() > MAX_OAUTH_RESPONSE_BYTES {
            return Err(OAuthError::protocol(
                OAuthOperation::Discovery,
                ProtocolViolation::ResponseTooLarge,
            ));
        }

        let wire: DiscoveryWire = serde_json::from_slice(body).map_err(|_| {
            OAuthError::protocol(OAuthOperation::Discovery, ProtocolViolation::InvalidJson)
        })?;

        let issuer_value = required(wire.issuer, "issuer")?;
        let issuer = config.validate_discovered_issuer(&issuer_value)?;
        let authorization_endpoint_value =
            required(wire.authorization_endpoint, "authorization_endpoint")?;
        let authorization_endpoint =
            config.validate_discovered_endpoint(&authorization_endpoint_value)?;
        let token_endpoint_value = required(wire.token_endpoint, "token_endpoint")?;
        let token_endpoint = config.validate_discovered_endpoint(&token_endpoint_value)?;
        let jwks_uri_value = required(wire.jwks_uri, "jwks_uri")?;
        let jwks_uri = config.validate_discovered_endpoint(&jwks_uri_value)?;
        let userinfo_endpoint_value = required(wire.userinfo_endpoint, "userinfo_endpoint")?;
        let userinfo_endpoint = config.validate_discovered_endpoint(&userinfo_endpoint_value)?;

        if wire
            .id_token_signing_alg_values_supported
            .iter()
            .any(|algorithm| algorithm.eq_ignore_ascii_case("none"))
        {
            return Err(OAuthError::protocol(
                OAuthOperation::Discovery,
                ProtocolViolation::InsecureSigningAlgorithm,
            ));
        }
        if wire.id_token_signing_alg_values_supported.is_empty() {
            return Err(OAuthError::protocol(
                OAuthOperation::Discovery,
                ProtocolViolation::MissingField("id_token_signing_alg_values_supported"),
            ));
        }

        Ok(Self {
            issuer,
            authorization_endpoint,
            token_endpoint,
            jwks_uri,
            userinfo_endpoint,
            signing_algorithms: wire.id_token_signing_alg_values_supported,
        })
    }

    /// Returns the validated issuer from the discovery document.
    #[must_use]
    pub fn issuer(&self) -> &Url {
        &self.issuer
    }

    /// Returns the validated authorization endpoint.
    #[must_use]
    pub fn authorization_endpoint(&self) -> &Url {
        &self.authorization_endpoint
    }

    /// Returns the validated token endpoint.
    #[must_use]
    pub fn token_endpoint(&self) -> &Url {
        &self.token_endpoint
    }

    /// Returns the validated JWKS endpoint required by an ID-token verifier.
    #[must_use]
    pub fn jwks_uri(&self) -> &Url {
        &self.jwks_uri
    }

    /// Returns the validated authoritative user-info endpoint.
    #[must_use]
    pub fn userinfo_endpoint(&self) -> &Url {
        &self.userinfo_endpoint
    }

    /// Returns algorithms advertised by discovery. The verifier must still
    /// enforce its own cryptographic allowlist.
    #[must_use]
    pub fn signing_algorithms(&self) -> &[String] {
        &self.signing_algorithms
    }
}

#[derive(Deserialize)]
struct DiscoveryWire {
    issuer: Option<String>,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    jwks_uri: Option<String>,
    userinfo_endpoint: Option<String>,
    #[serde(default)]
    id_token_signing_alg_values_supported: Vec<String>,
}

fn required(value: Option<String>, field: &'static str) -> Result<String, OAuthError> {
    value.filter(|value| !value.is_empty()).ok_or_else(|| {
        OAuthError::protocol(
            OAuthOperation::Discovery,
            ProtocolViolation::MissingField(field),
        )
    })
}
