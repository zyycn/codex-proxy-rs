//! Dashboard、用量与诊断查询的 wire 映射和固定路由。

use std::collections::BTreeMap;

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use gateway_admin::model::{PageSize as DomainPageSize, observability as domain};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::presenter::{format_compact_number, format_decimal_currency, format_number};
use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminQuery, AdminResponse, AdminSessionState,
    WireValidationError, wire::map_admin_service_error,
};

mod presenter;
mod query;
mod routes;
mod wire;

pub(crate) use presenter::*;
pub use query::*;
pub use routes::*;
pub use wire::*;
