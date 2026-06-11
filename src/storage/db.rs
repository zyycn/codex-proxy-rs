use std::{str::FromStr, time::Duration};

use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};

pub async fn connect_sqlite(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    // 中文注释：WAL 和外键必须在连接层统一启用，避免后续仓储绕过一致性约束。
    let pool = SqlitePool::connect_with(options).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
