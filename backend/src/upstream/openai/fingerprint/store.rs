//! OpenAI 客户端指纹 PostgreSQL 存储。

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::types::Fingerprint;

pub const CURRENT_FINGERPRINT_ID: &str = "current";
const AUTO_UPDATE_SOURCE: &str = "auto_update";
const CONFIG_SEED_SOURCE: &str = "config_seed";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredHeader {
    name: String,
    value: String,
}

/// 指纹自动更新状态。

#[derive(Clone)]
pub struct PgFingerprintStore {
    pool: PgPool,
}

impl PgFingerprintStore {
    /// 使用给定连接池构造仓储。
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 写入当前指纹默认值；如果当前槽位已存在，直接读取数据库值。
    pub async fn ensure_current_seed(
        &self,
        default_fingerprint: &Fingerprint,
    ) -> Result<Fingerprint, sqlx::Error> {
        if let Some(fingerprint) = self.load_current().await? {
            return Ok(fingerprint);
        }

        let now = Utc::now();
        sqlx::query(
            r"
            insert into fingerprints (
              id,
              originator,
              app_version,
              build_number,
              platform,
              arch,
              chromium_version,
              user_agent_template,
              default_headers_json,
              header_order_json,
              source,
              created_at,
              updated_at
            ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)
            on conflict (id) do nothing
            ",
        )
        .bind(CURRENT_FINGERPRINT_ID)
        .bind(&default_fingerprint.originator)
        .bind(&default_fingerprint.app_version)
        .bind(&default_fingerprint.build_number)
        .bind(&default_fingerprint.platform)
        .bind(&default_fingerprint.arch)
        .bind(&default_fingerprint.chromium_version)
        .bind(&default_fingerprint.user_agent_template)
        .bind(sqlx::types::Json(encode_default_headers(
            &default_fingerprint.default_headers,
        )))
        .bind(sqlx::types::Json(&default_fingerprint.header_order))
        .bind(CONFIG_SEED_SOURCE)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.load_current().await?.ok_or(sqlx::Error::RowNotFound)
    }

    /// 更新当前指纹中的自动更新版本字段。
    pub async fn update_current_version(
        &self,
        app_version: &str,
        build_number: &str,
        chromium_version: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let resolved_chromium_version = match chromium_version {
            Some(version) => version.to_string(),
            None => {
                sqlx::query_scalar::<_, String>(
                    "select chromium_version from fingerprints where id = $1",
                )
                .bind(CURRENT_FINGERPRINT_ID)
                .fetch_one(&self.pool)
                .await?
            }
        };

        let result = sqlx::query(
            "update fingerprints set app_version = $1, build_number = $2, chromium_version = $3, source = $4, updated_at = $5 where id = $6",
        )
        .bind(app_version)
        .bind(build_number)
        .bind(resolved_chromium_version)
        .bind(AUTO_UPDATE_SOURCE)
        .bind(Utc::now())
        .bind(CURRENT_FINGERPRINT_ID)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(sqlx::Error::RowNotFound);
        }
        Ok(())
    }

    /// 读取当前运行时指纹。
    pub async fn load_current(&self) -> Result<Option<Fingerprint>, sqlx::Error> {
        let row = sqlx::query(
            r"
            select
              originator,
              app_version,
              build_number,
              platform,
              arch,
              chromium_version,
              user_agent_template,
              default_headers_json,
              header_order_json,
              updated_at
            from fingerprints
            where id = $1
            ",
        )
        .bind(CURRENT_FINGERPRINT_ID)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| fingerprint_from_row(&row)).transpose()
    }

    /// 记录自动更新历史。
    pub async fn insert_update_history(
        &self,
        app_version: &str,
        build_number: &str,
        chromium_version: Option<&str>,
        manifest_json: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let manifest = manifest_json
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .map_err(json_error)?;
        sqlx::query(
            "insert into fingerprint_update_history (id, current_fingerprint_id, app_version, build_number, chromium_version, source, manifest_json, created_at) values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(CURRENT_FINGERPRINT_ID)
        .bind(app_version)
        .bind(build_number)
        .bind(chromium_version)
        .bind(AUTO_UPDATE_SOURCE)
        .bind(manifest.map(sqlx::types::Json))
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn fingerprint_from_row(row: &sqlx::postgres::PgRow) -> Result<Fingerprint, sqlx::Error> {
    let default_headers = row
        .get::<sqlx::types::Json<Vec<StoredHeader>>, _>("default_headers_json")
        .0;
    let header_order = row
        .get::<sqlx::types::Json<Vec<String>>, _>("header_order_json")
        .0;
    let updated_at = row.get::<DateTime<Utc>, _>("updated_at");
    Ok(Fingerprint {
        originator: row.get("originator"),
        app_version: row.get("app_version"),
        build_number: row.get("build_number"),
        platform: row.get("platform"),
        arch: row.get("arch"),
        chromium_version: row.get("chromium_version"),
        user_agent_template: row.get("user_agent_template"),
        default_headers: decode_default_headers(default_headers),
        header_order,
        updated_at: Some(updated_at.to_rfc3339()),
    })
}

fn encode_default_headers(headers: &IndexMap<String, String>) -> Vec<StoredHeader> {
    headers
        .iter()
        .map(|(name, value)| StoredHeader {
            name: name.clone(),
            value: value.clone(),
        })
        .collect()
}

fn decode_default_headers(headers: Vec<StoredHeader>) -> IndexMap<String, String> {
    headers
        .into_iter()
        .map(|header| (header.name, header.value))
        .collect()
}

fn json_error(error: serde_json::Error) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(error))
}
