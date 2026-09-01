//! 管理端账号目录的 HTTP 合同、凭据动作、路由处理与安全响应投影。

use std::{collections::BTreeSet, convert::Infallible, fmt};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, FixedOffset, Utc};
use futures::{Stream, StreamExt as _};
use gateway_admin::model::{
    AdminError as AdminServiceError, PageSize,
    accounts::{
        AccountConcurrencyLimit, AccountConnectionTestEvent as DomainConnectionTestEvent,
        AccountCost, AccountGroupFilter, AccountListQuery, AccountModelUsage, AccountSort,
        AccountSortField, AccountStatus as DomainAccountStatus, AccountUpdateResult, AccountUsage,
        AccountWeight, AccountsUpdateResult, BatchUpdateAccounts, SortDirection, UpdateAccount,
    },
    provider_credentials::{
        AccountDirectoryItem, AccountDirectoryPage, AccountExportBundle, AccountRefreshResult,
        AuthorizationStarted, CompleteAuthorization, ConsumeProviderResetCredit,
        CredentialDeletion, CredentialDeletionResult, CredentialImportResult, CredentialMutation,
        CredentialMutationResult, ImportCredentials, ProviderDocument, ProviderModels,
        ProviderProfileActivityInsights, ProviderProfileAvatar, ProviderProfileDailyUsage,
        ProviderProfileInvocation, ProviderProfileStatistics, ProviderProfileStatisticsSummary,
        ProviderQuota, ProviderQuotaWindow, ProviderResetCredit, ProviderResetCreditResult,
        ProviderResetCredits, RotateCredential, StartAuthorization,
    },
};
use gateway_core::{
    account::{OpaqueProviderData, ProviderAccountId},
    routing::{AccountGroupId, ProviderKind, UpstreamModelId},
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use uuid::{Uuid, Version};

use super::presenter::{format_compact_number, format_decimal_currency, format_number};
use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse, AdminSessionState,
    PageMeta, WireValidationError,
};

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;
const MAX_SEARCH_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 512;
const MAX_IMPORT_DATA_BYTES: usize = 64 * 1024 * 1024;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 64 * 1024;
const MAX_ID_TOKEN_BYTES: usize = 16 * 1024;
const MAX_CALLBACK_URL_BYTES: usize = 64 * 1024;
const MAX_ACCOUNT_DELETE_BATCH: usize = 200;
const MAX_ACCOUNT_GROUP_BATCH: usize = 1000;
const MAX_AVATAR_VERSION_BYTES: usize = 32;

mod credentials;
mod handlers;
mod presenter;
mod wire;

pub use credentials::*;
pub use handlers::{profile_avatar_response, router};
pub use wire::*;

use credentials::{
    AccountProvider, deserialize_required_nullable, parse_account_weight, parse_concurrency_limit,
    provider_document_value, require_account_id, validate_wire_group_ids,
};
use presenter::*;
use wire::BatchUpdatedAccountsData;
