//! 明文 `provider_accounts` 与凭证 revision CAS 的唯一 PostgreSQL owner。

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
    time::SystemTime,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::{
    model::{
        MutationContext, Revision as AdminRevision,
        accounts::{
            AccountCost, AccountListQuery as AdminAccountListQuery, AccountModelUsage, AccountPage,
            AccountRecord, AccountRequestBucket, AccountSort as AdminAccountSort,
            AccountSortField as AdminAccountSortField, AccountStatus as AdminAccountStatus,
            AccountSummary, AccountUsage, AccountUsageWindowQuery, AccountUsageWindowResult,
            DeleteAccounts, SetAccountEnabled, SortDirection as AdminSortDirection,
        },
        observability::{
            CostCoverage as AdminCostCoverage, DecimalAmount as AdminDecimalAmount, TimeRange,
        },
        provider_credentials::{
            AuthorizationCommit, AuthorizationCredentialCommit, CredentialCursor,
            CredentialDetails, CredentialImportCommit, CredentialImportResult, CredentialListQuery,
            CredentialListWindow, CredentialMutationResult, CredentialPage,
            CredentialRotationCommit, PreparedCredentialCreate, PreparedCredentialImport,
            PreparedCredentialRotationFacts, ProviderDocument, ProviderExportCredentialInput,
        },
    },
    ports::store::{AccountStore, AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
};
use sqlx::{PgPool, Postgres, Row, Transaction};

use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
    CredentialRevision as CoreCredentialRevision, LoadedCredential,
    NewProviderAccount as CoreNewProviderAccount, OpaqueProviderData, PlaintextCredential,
    ProviderAccount as CoreProviderAccount, ProviderAccountId as CoreProviderAccountId,
    ProviderAccountIdentity, ProviderAccountStore,
    ProviderAccountUpdate as CoreProviderAccountUpdate, QuotaObservation, QuotaWriteOutcome,
};
use gateway_core::error::{StoreError as CoreStoreError, StoreErrorKind as CoreStoreErrorKind};
use gateway_core::routing::ProviderKind;

use crate::{
    ConflictKind, JsonObject, Revision, StoreError, StoreResult, admin_revision, admin_store_error,
    mutation_audit, postgres_unavailable, require_nonempty, store_revision,
};

use super::{
    AdminAuditEvent, ControlPlaneRepository, CurrencyCostTotal, ObservabilityRange,
    ObservabilityRepository, PgControlPlaneRepository, PgObservabilityRepository,
    ProviderAccountModelUsageObservation, ProviderAccountUsageObservation,
    ProviderAccountUsageQuery, append_admin_audit_event_in_transaction,
    bump_config_revision_in_transaction,
};

mod admin_adapter;
mod core_adapter;
mod mapping;
mod repository;
mod rows;

pub use admin_adapter::*;
pub(crate) use mapping::*;
pub use repository::*;
pub use rows::*;
