//! 从 `model_requests`、`ops_events` 与账号公共投影读取观测事实。

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    str::FromStr,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::{
    model::observability as admin_observability,
    ports::store::{AdminStoreResult, ObservabilityStore as AdminObservabilityStore},
};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};

use crate::{
    DecimalAmount, StoreError, StoreResult, admin_store_error, postgres_unavailable,
    require_nonempty,
};

use super::{completed_usage_fact_predicate, push_completed_usage_fact_filter};

mod admin_adapter;
mod mapping;
mod model;
mod queries;
mod query_budget;

pub use admin_adapter::*;
pub(crate) use mapping::*;
pub use model::*;
pub(crate) use queries::*;
pub use query_budget::*;
