//! Codex credential domain exports。

mod admin;
mod catalog;
mod cookie;
mod identity;
mod oauth;
mod quota;
mod refresh;
mod repository;
mod security;
mod selector;
pub mod token_client;
mod types;

pub(crate) use admin::refresh_time;
pub(crate) use oauth::oauth_owner_ref;

pub use admin::{
    CodexCprExportDocument, CodexCredentialAdmin, CodexCredentialAdminError,
    CodexCredentialAdminService, ExportManagedCodexCredential, ImportCodexOAuthCredential,
    ImportCodexOAuthCredentialBatch, PreparedCodexAccountImport, PreparedCodexCredentialRotation,
    PreparedCodexCredentialRotationGuard, RotateManagedCodexCredential,
};
pub use catalog::{
    CodexCredentialCatalogError, CodexCredentialCatalogService, CodexCredentialCatalogSnapshot,
};
pub use cookie::{CodexCookiePolicy, CookiePolicyError};
pub use identity::{
    CodexAccountIdentityService, CodexAccountIdentityVerifier, CodexAuthenticatedAccount,
    CodexAuthenticatedAccountSource, CodexIdentityExpectation, CodexIdentityVerification,
    CodexIdentityVerificationError, CodexJwksSource, CodexJwtIdentityVerifier, CodexSignedIdentity,
    CodexSignedIdentityVerifier, OFFICIAL_OPENAI_API_AUDIENCE, OFFICIAL_OPENAI_ISSUER,
    OFFICIAL_OPENAI_JWKS_URI, ReqwestCodexAuthenticatedAccountSource, ReqwestOpenAiJwksSource,
};
pub use oauth::{
    CodexOAuthAdmin, CodexOAuthAdminError, CodexOAuthAdminService, CodexOAuthAuthorizationStarted,
    CodexOAuthPendingStore, CodexOAuthPendingStoreError, CodexOAuthReauthorizationTarget,
    CodexPendingAuthorization, CompleteCodexOAuthAuthorization, CompletedCodexOAuthAuthorization,
    CompletedCodexOAuthCredential, StartCodexOAuthAuthorization, StoredCodexPendingAuthorization,
};
pub use quota::{
    CodexAccountQuotaSnapshot, CodexCredentialQuotaError, CodexCredentialQuotaService,
    CodexQuotaFact, CodexQuotaSyncSummary, CodexQuotaWindow, CodexQuotaWindowKind,
    CodexQuotaWindowRole, parse_codex_quota_usage,
};
pub use refresh::{
    CodexCredentialRefreshError, CodexCredentialRefreshOutcome, CodexCredentialRefreshService,
    DueCodexCredential,
};
pub use repository::{CodexCredentialRepository, CredentialRepositoryError};
pub use security::{CodexCredentialCodec, CodexCredentialDataError, CodexRuntimeCredential};
pub use selector::{
    CodexAccountFailure, CodexCredentialLease, CodexCredentialSelector, CredentialSelectionError,
    SelectCodexCredential,
};
pub use types::{
    CodexAccountProfile, CodexCookie, CodexCookieCaptureOutcome, CodexCredentialData,
    CodexCredentialPrincipal, CodexOAuthSecret, CreateCodexCredential, CredentialRecord,
    RotateCodexCredential, RuntimeCodexCookie, UpsertCodexCookie,
};
