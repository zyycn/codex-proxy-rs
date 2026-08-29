//! 多 Provider 网关的 PostgreSQL 持久化与 Redis 协调 adapter。
//!
//! 业务规则与 port 由 `gateway-core` / `gateway-admin` 拥有。本 crate 只负责把 PostgreSQL 业务表
//! 和可丢失 Redis 状态映射为明确的基础设施操作。

use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{fmt, num::NonZeroU64, str::FromStr};

use gateway_admin::model::auth::{AdminAuditEvent as AdminAuditModel, AdminSession};
use gateway_admin::model::settings::{
    AdminApiKey, AdminApiKeyMutation, ModelMappings, ReplaceRuntimeSettings,
    RotationStrategy as AdminRotationStrategy, RuntimeSettings as AdminRuntimeSettings,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision as AdminRevision};
use gateway_admin::ports::backup::BackupStorePorts;
use gateway_admin::ports::store::{
    AdminStoreError, AdminStoreErrorKind, AdminStorePorts, AdminStoreResult, AuthStore,
    SettingsStore,
};
use gateway_core::CoreStorePorts;
use gateway_core::health::{HealthProbe, HealthState};
use gateway_core::provider_ports::ProviderStorePorts;
use gateway_core::task::{
    DaemonRestartPolicy, ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerId,
    WorkerKind, WorkerLeaderLeasePort, WorkerLeaseRequest, WorkerRegistration, WorkerRunnable,
    WorkerSchedule, WorkerTaskError,
};
use serde::Deserialize;
use serde_json::{Map, Value};

mod admin_adapter;
mod bundle;
mod config;
mod value;
mod workers;

pub mod backup;
pub mod postgres;
pub mod redis;

pub(crate) use admin_adapter::*;
pub use bundle::*;
pub use config::*;
pub use value::*;
pub(crate) use workers::*;
