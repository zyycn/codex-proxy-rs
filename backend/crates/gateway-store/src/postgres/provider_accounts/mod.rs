//! 明文 `provider_accounts` 与凭证 revision CAS 的唯一 PostgreSQL owner

use std::{collections::BTreeSet, fmt};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::{
    model::{
        MutationContext, Revision as AdminRevision,
        account_groups::AccountGroupRef,
        accounts::{
            AccountGroupFilter, AccountImportSettings, AccountListQuery as AdminAccountListQuery,
            AccountPage, AccountPageItem, AccountRecord, AccountSort as AdminAccountSort,
            AccountSortField as AdminAccountSortField, AccountSummary, AccountUpdateResult,
            AccountUsage, AccountUsageWindowQuery, AccountUsageWindowResult, AccountsUpdateResult,
            BatchUpdateAccounts, DeleteAccounts, SortDirection as AdminSortDirection,
            UpdateAccount,
        },
        observability::TimeRange,
        provider_credentials::{
            AuthorizationCommit, AuthorizationCredentialCommit, CredentialDetails,
            CredentialImportCommit, CredentialImportResult, CredentialMutationResult,
            CredentialRotationCommit, PluginAccountListQuery, PluginAccountPage,
            PreparedCredentialCreate, PreparedCredentialImport, PreparedCredentialRotationFacts,
            ProviderDocument, ProviderExportCredentialInput,
        },
    },
    ports::store::{AccountStore, AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
};
use gateway_core::account::{
    AccountConcurrencyLimit, AccountErrorReason, AccountStateChange, AccountWeight,
    CredentialCasOutcome, CredentialCasUpdate, CredentialCasUpdateParts,
    CredentialRevision as CoreCredentialRevision, CredentialState, LoadedCredential,
    NewProviderAccount as CoreNewProviderAccount, OpaqueProviderData, PlaintextCredential,
    ProviderAccount as CoreProviderAccount, ProviderAccountId as CoreProviderAccountId,
    ProviderAccountIdentity, ProviderAccountStore,
    ProviderAccountUpdate as CoreProviderAccountUpdate,
    ProviderRefreshQuery as CoreProviderRefreshQuery, QuotaAccessChange, QuotaAccessState,
    QuotaEvidence, QuotaObservation, QuotaObservationTouch, QuotaState, QuotaWriteOutcome,
};
use gateway_core::error::{StoreError as CoreStoreError, StoreErrorKind as CoreStoreErrorKind};
use gateway_core::routing::{AccountGroupId, ProviderKind};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{
    ConflictKind, JsonObject, Revision, StoreError, StoreResult, admin_revision, admin_store_error,
    mutation_audit, postgres_unavailable, require_nonempty, store_revision,
};

use super::{
    AdminAuditEvent, ControlPlaneRepository, ObservabilityRange, PgControlPlaneRepository,
    PgObservabilityRepository, ProviderAccountUsageObservation, ProviderAccountUsageQuery,
    append_admin_audit_event_in_transaction, bump_config_revision_in_transaction,
};

mod admin_adapter;
mod admin_queries;
mod authorization;
mod core_adapter;
mod mapping;
mod quota_forecast;
mod repository;
mod reset_credit_automation;
mod rows;
mod runtime;

pub use admin_adapter::*;
pub(crate) use admin_queries::*;
pub(crate) use mapping::*;
pub use repository::*;
pub use rows::*;
pub(crate) use runtime::*;
