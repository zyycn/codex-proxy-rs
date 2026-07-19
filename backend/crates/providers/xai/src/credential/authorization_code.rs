use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::credential::pkce::Pkce;
use crate::{
    AllowedRedirectUri, CallbackRejection, DiscoveryDocument, GrokOAuthConfig, OAuthError,
    OAuthPrincipal, SecretValue,
};

const MAX_CALLBACK_VALUE_BYTES: usize = 64 * 1024;

/// Parsed callback whose code and state remain redacted in debug output.
pub struct AuthorizationCallback {
    code: Option<SecretValue>,
    state: Option<SecretValue>,
    rejection: Option<CallbackRejection>,
}

impl AuthorizationCallback {
    /// Parses an OAuth callback query while rejecting duplicate security fields.
    /// Raw server descriptions are intentionally discarded.
    ///
    /// # Errors
    ///
    /// Returns [`CallbackRejection::DuplicateParameter`] for repeated `code`,
    /// `state`, or `error` keys.
    pub fn parse(query: &str) -> Result<Self, CallbackRejection> {
        let mut code = None;
        let mut state = None;
        let mut rejection = None;
        let query = query.strip_prefix('?').unwrap_or(query);

        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match name.as_ref() {
                "code" => insert_once(&mut code, value.into_owned())?,
                "state" => insert_once(&mut state, value.into_owned())?,
                "error" => {
                    if rejection.is_some() {
                        return Err(CallbackRejection::DuplicateParameter);
                    }
                    rejection = Some(if value == "access_denied" {
                        CallbackRejection::AccessDenied
                    } else {
                        CallbackRejection::ProviderRejected
                    });
                }
                _ => {}
            }
        }

        Ok(Self {
            code: code.map(SecretValue::new),
            state: state.map(SecretValue::new),
            rejection,
        })
    }
}

impl fmt::Debug for AuthorizationCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizationCallback")
            .field("code", &self.code.as_ref().map(|_| "[REDACTED]"))
            .field("state", &self.state.as_ref().map(|_| "[REDACTED]"))
            .field("rejection", &self.rejection)
            .finish()
    }
}

/// Authorization Code + PKCE state awaiting one callback.
pub struct PendingAuthorization {
    authorization_url: Url,
    redirect_uri: AllowedRedirectUri,
    state: SecretValue,
    nonce: SecretValue,
    code_verifier: SecretValue,
}

impl PendingAuthorization {
    pub fn start(
        config: &GrokOAuthConfig,
        discovery: &DiscoveryDocument,
        redirect_uri: AllowedRedirectUri,
        principal: Option<&OAuthPrincipal>,
    ) -> Result<Self, OAuthError> {
        let pkce = Pkce::generate()?;
        let state = random_urlsafe_secret()?;
        let nonce = random_urlsafe_secret()?;
        let mut authorization_url = discovery.authorization_endpoint().clone();

        {
            let mut query = authorization_url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", config.client_id())
                .append_pair("redirect_uri", redirect_uri.as_url().as_str())
                .append_pair("scope", &config.scope_string())
                .append_pair("code_challenge", pkce.challenge())
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", state.expose())
                .append_pair("nonce", nonce.expose())
                .append_pair("plan", "generic");
            if let Some(principal) = principal {
                query
                    .append_pair("principal_type", principal.principal_type())
                    .append_pair("principal_id", principal.principal_id());
            }
            query.append_pair("referrer", "codex-proxy-rs");
        }

        Ok(Self {
            authorization_url,
            redirect_uri,
            state,
            nonce,
            code_verifier: pkce.into_verifier(),
        })
    }

    /// Returns the URL that an administrator opens at the official issuer.
    #[must_use]
    pub fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    /// 将 server-only PKCE 状态编码为待加密载荷；调用方必须在离开内存前加密。
    ///
    /// # Errors
    ///
    /// 内部状态无法序列化时 fail closed。
    pub fn into_server_state(self) -> Result<SecretValue, OAuthError> {
        let wire = PendingAuthorizationWire {
            schema_version: 1,
            authorization_url: self.authorization_url.as_str(),
            redirect_uri: self.redirect_uri.as_url().as_str(),
            state: self.state.expose(),
            nonce: self.nonce.expose(),
            code_verifier: self.code_verifier.expose(),
        };
        serde_json::to_string(&wire)
            .map(SecretValue::new)
            .map_err(|_| pending_state_error())
    }

    /// 从已经通过服务端 envelope 认证的载荷恢复一次性 PKCE 状态。
    ///
    /// # Errors
    ///
    /// 版本、URL 或 secret 形态不满足固定官方协议时拒绝恢复。
    pub fn from_server_state(
        config: &GrokOAuthConfig,
        state: &SecretValue,
    ) -> Result<Self, OAuthError> {
        let mut wire: OwnedPendingAuthorizationWire =
            serde_json::from_str(state.expose()).map_err(|_| pending_state_error())?;
        if wire.schema_version != 1
            || !valid_urlsafe_secret(&wire.state)
            || !valid_urlsafe_secret(&wire.nonce)
            || !valid_urlsafe_secret(&wire.code_verifier)
        {
            return Err(pending_state_error());
        }
        let authorization_url =
            Url::parse(&wire.authorization_url).map_err(|_| pending_state_error())?;
        if !valid_restored_authorization_url(config, &authorization_url, &wire) {
            return Err(pending_state_error());
        }
        let redirect_uri = AllowedRedirectUri::restore_server_side(&wire.redirect_uri)?;
        Ok(Self {
            authorization_url,
            redirect_uri,
            state: SecretValue::new(std::mem::take(&mut wire.state)),
            nonce: SecretValue::new(std::mem::take(&mut wire.nonce)),
            code_verifier: SecretValue::new(std::mem::take(&mut wire.code_verifier)),
        })
    }

    /// Consumes the one-time flow and validates mandatory callback state.
    ///
    /// # Errors
    ///
    /// Returns a callback rejection for missing/mismatched state, provider
    /// denial, or a missing code.
    pub fn accept_callback(
        self,
        callback: AuthorizationCallback,
    ) -> Result<AuthorizationCodeGrant, OAuthError> {
        let callback_state = callback.state.ok_or(CallbackRejection::MissingState)?;
        if !self.state.constant_time_eq(&callback_state) {
            return Err(CallbackRejection::StateMismatch.into());
        }
        if let Some(rejection) = callback.rejection {
            return Err(rejection.into());
        }
        let code = callback.code.ok_or(CallbackRejection::MissingCode)?;
        if code.is_empty()
            || code.len() > MAX_CALLBACK_VALUE_BYTES
            || code.expose().chars().any(char::is_control)
        {
            return Err(CallbackRejection::ProviderRejected.into());
        }

        Ok(AuthorizationCodeGrant {
            code,
            redirect_uri: self.redirect_uri,
            code_verifier: self.code_verifier,
            nonce: self.nonce,
        })
    }
}

#[derive(Serialize)]
struct PendingAuthorizationWire<'a> {
    schema_version: u32,
    authorization_url: &'a str,
    redirect_uri: &'a str,
    state: &'a str,
    nonce: &'a str,
    code_verifier: &'a str,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
struct OwnedPendingAuthorizationWire {
    schema_version: u32,
    authorization_url: String,
    redirect_uri: String,
    state: String,
    nonce: String,
    code_verifier: String,
}

impl fmt::Debug for PendingAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingAuthorization")
            .field("authorization_url", &"[REDACTED: contains state and nonce]")
            .field("redirect_uri", &self.redirect_uri)
            .field("state", &"[REDACTED]")
            .field("nonce", &"[REDACTED]")
            .field("code_verifier", &"[REDACTED]")
            .finish()
    }
}

/// State-validated authorization grant ready for one token exchange.
pub struct AuthorizationCodeGrant {
    code: SecretValue,
    redirect_uri: AllowedRedirectUri,
    code_verifier: SecretValue,
    nonce: SecretValue,
}

impl AuthorizationCodeGrant {
    pub(crate) fn into_parts(self) -> (SecretValue, AllowedRedirectUri, SecretValue, SecretValue) {
        (self.code, self.redirect_uri, self.code_verifier, self.nonce)
    }
}

impl fmt::Debug for AuthorizationCodeGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizationCodeGrant")
            .field("code", &"[REDACTED]")
            .field("redirect_uri", &self.redirect_uri)
            .field("code_verifier", &"[REDACTED]")
            .field("nonce", &"[REDACTED]")
            .finish()
    }
}

fn random_urlsafe_secret() -> Result<SecretValue, OAuthError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| OAuthError::EntropyUnavailable)?;
    Ok(SecretValue::new(URL_SAFE_NO_PAD.encode(random)))
}

fn valid_urlsafe_secret(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|decoded| decoded.len() == 32)
}

fn valid_restored_authorization_url(
    config: &GrokOAuthConfig,
    url: &Url,
    wire: &OwnedPendingAuthorizationWire,
) -> bool {
    if url.scheme() != "https"
        || url.host_str() != config.issuer().host_str()
        || url.port_or_known_default() != Some(443)
        || url.path() != "/oauth2/authorize"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }

    let mut state = None;
    let mut nonce = None;
    let mut redirect_uri = None;
    let mut client_id = None;
    for (name, value) in url.query_pairs() {
        let target = match name.as_ref() {
            "state" => &mut state,
            "nonce" => &mut nonce,
            "redirect_uri" => &mut redirect_uri,
            "client_id" => &mut client_id,
            _ => continue,
        };
        if target.replace(value.into_owned()).is_some() {
            return false;
        }
    }
    state.as_deref() == Some(wire.state.as_str())
        && nonce.as_deref() == Some(wire.nonce.as_str())
        && redirect_uri.as_deref() == Some(wire.redirect_uri.as_str())
        && client_id.as_deref() == Some(config.client_id())
}

fn pending_state_error() -> OAuthError {
    OAuthError::protocol(
        crate::OAuthOperation::AuthorizationCodeToken,
        crate::ProtocolViolation::InvalidField("pending_authorization"),
    )
}

fn insert_once(slot: &mut Option<String>, value: String) -> Result<(), CallbackRejection> {
    if slot.is_some() || value.len() > MAX_CALLBACK_VALUE_BYTES {
        return Err(CallbackRejection::DuplicateParameter);
    }
    *slot = Some(value);
    Ok(())
}
