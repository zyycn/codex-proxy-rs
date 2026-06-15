use chrono::Utc;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::codex::gateway::fingerprint::{
    model::Fingerprint,
    updater::{FingerprintUpdate, CODEX_DESKTOP_UPDATE_SOURCE},
};

const AUTO_UPDATED_FINGERPRINT_ID: &str = "auto_updated";
const AUTO_UPDATE_SOURCE: &str = "auto_update";
const AUTO_UPDATE_PLATFORM: &str = "darwin";
const AUTO_UPDATE_ARCH: &str = "arm64";
const AUTO_UPDATE_USER_AGENT_TEMPLATE: &str = "Codex Desktop/{version} ({platform}; {arch})";
const DEFAULT_AUTO_UPDATE_CHROMIUM_VERSION: &str = "146";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFingerprint {
    pub app_version: String,
    pub build_number: String,
    pub source: String,
}

#[derive(Clone)]
pub struct FingerprintRepository {
    pool: SqlitePool,
}

impl FingerprintRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert_update(&self, update: &FingerprintUpdate) -> Result<(), sqlx::Error> {
        let mut fp = Fingerprint::default_codex_desktop();
        fp.app_version.clone_from(&update.app_version);
        fp.build_number.clone_from(&update.build_number);
        sqlx::query(
            "insert into fingerprints (id, app_version, build_number, platform, arch, chromium_version, user_agent_template, source, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(fp.app_version)
        .bind(fp.build_number)
        .bind(fp.platform)
        .bind(fp.arch)
        .bind(fp.chromium_version)
        .bind(fp.user_agent_template)
        .bind(CODEX_DESKTOP_UPDATE_SOURCE)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_auto_update(
        &self,
        app_version: &str,
        build_number: &str,
        chromium_version: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "insert into fingerprints (id, app_version, build_number, platform, arch, chromium_version, user_agent_template, source, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             on conflict(id) do update set app_version = excluded.app_version, build_number = excluded.build_number, platform = excluded.platform, arch = excluded.arch, chromium_version = excluded.chromium_version, user_agent_template = excluded.user_agent_template, source = excluded.source, created_at = excluded.created_at",
        )
        .bind(AUTO_UPDATED_FINGERPRINT_ID)
        .bind(app_version)
        .bind(build_number)
        .bind(AUTO_UPDATE_PLATFORM)
        .bind(AUTO_UPDATE_ARCH)
        .bind(chromium_version.unwrap_or(DEFAULT_AUTO_UPDATE_CHROMIUM_VERSION))
        .bind(AUTO_UPDATE_USER_AGENT_TEMPLATE)
        .bind(AUTO_UPDATE_SOURCE)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn latest(&self) -> Result<Option<StoredFingerprint>, sqlx::Error> {
        let row = sqlx::query(
            "select app_version, build_number, source from fingerprints order by created_at desc, id desc limit 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| StoredFingerprint {
            app_version: row.get("app_version"),
            build_number: row.get("build_number"),
            source: row.get("source"),
        }))
    }

    pub async fn load_latest_auto_updated(&self) -> Result<Option<Fingerprint>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            select app_version, build_number, platform, arch, chromium_version, user_agent_template
            from fingerprints
            where source = ?
            order by created_at desc
            limit 1
            "#,
        )
        .bind(AUTO_UPDATE_SOURCE)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        Ok(Some(Fingerprint {
            originator: "Codex Desktop".to_string(),
            app_version: row.get("app_version"),
            build_number: row.get("build_number"),
            platform: row.get("platform"),
            arch: row.get("arch"),
            chromium_version: row.get("chromium_version"),
            user_agent_template: row.get("user_agent_template"),
            default_headers: Fingerprint::default_headers(),
            header_order: Fingerprint::default_header_order(),
        }))
    }
}
