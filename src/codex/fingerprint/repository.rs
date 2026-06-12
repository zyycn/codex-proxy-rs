use chrono::Utc;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::codex::fingerprint::{
    model::Fingerprint,
    updater::{FingerprintUpdate, CODEX_DESKTOP_UPDATE_SOURCE},
};

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
}
