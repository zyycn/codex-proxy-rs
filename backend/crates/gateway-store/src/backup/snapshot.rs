//! 与业务数据共用快照导出已发布的小时总量，不扫描请求明细或逐请求投影

use futures::TryStreamExt as _;
use gateway_admin::model::backup::{BackupError, code};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::io::{AsyncWrite, AsyncWriteExt as _};

use super::pg_dump::{pg_dump_failed, write_failed};

/// 只排除表数据，保留完整 schema 及迁移历史；费用窗口独立于逐请求去重记录保留
const EXCLUDED_DATA: &[&str] = &[
    "model_requests",
    "model_request_leases",
    "ops_events",
    "admin_audit_events",
    "client_api_key_charge_events",
    "request_analytics_facts",
    "request_analytics_15m",
    "request_analytics_1h",
    "request_analytics_1d_counts",
    "request_analytics_pending_requests",
    "request_analytics_pending_buckets",
    "request_analytics_build_state",
];

pub(super) struct BusinessSnapshot {
    transaction: Transaction<'static, Postgres>,
    snapshot_id: String,
    schema: String,
}

impl BusinessSnapshot {
    pub async fn begin(pool: &PgPool, backup_id: &str) -> Result<Self, BackupError> {
        let mut transaction = pool
            .begin()
            .await
            .map_err(|_| pg_dump_failed("无法开始备份快照"))?;
        sqlx::query("set transaction isolation level repeatable read, read only")
            .execute(&mut *transaction)
            .await
            .map_err(|_| pg_dump_failed("无法隔离备份快照"))?;
        sqlx::query("set local statement_timeout = '30s'")
            .execute(&mut *transaction)
            .await
            .map_err(|_| pg_dump_failed("无法设置备份查询期限"))?;
        if !crate::postgres::backup_projection_ready(&mut transaction)
            .await
            .map_err(|_| pg_dump_failed("无法读取统计发布状态"))?
        {
            return Err(BackupError::new(
                code::ANALYTICS_NOT_READY,
                "用量统计正在重建，请在统计就绪后重试备份".to_owned(),
            ));
        }
        sqlx::query("select set_config('cpr.backup_id',$1,true)")
            .bind(backup_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| pg_dump_failed("无法标记备份汇总"))?;
        let (snapshot_id, schema): (String, String) =
            sqlx::query_as("select pg_export_snapshot(), current_schema()")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| pg_dump_failed("无法导出一致性快照"))?;
        Ok(Self {
            transaction,
            snapshot_id,
            schema: format!("\"{}\"", schema.replace('"', "\"\"")),
        })
    }

    pub fn configure(&self, command: &mut tokio::process::Command) {
        command
            .arg("--snapshot")
            .arg(&self.snapshot_id)
            .arg("--schema")
            .arg(&self.schema);
        for table in EXCLUDED_DATA {
            command
                .arg("--exclude-table-data")
                .arg(format!("{}.\"{table}\"", self.schema));
        }
    }

    pub fn restore_preamble(&self) -> String {
        // 新库自带 public；只替换空 schema，含已有对象时由 PostgreSQL 拒绝删除
        format!("DROP SCHEMA IF EXISTS {};\n", self.schema)
    }

    pub async fn append(
        mut self,
        writer: &mut (impl AsyncWrite + Unpin),
    ) -> Result<(), BackupError> {
        let header = format!(
            "\n-- CPR 业务备份：已发布的全局小时用量与维护贡献\nCOPY {}.request_analytics_1h (bucket_start,collection,dimensions,outcome,schema_version,histogram_version,counts,costs,distributions) FROM stdin;\n",
            self.schema
        );
        writer
            .write_all(header.as_bytes())
            .await
            .map_err(write_failed)?;
        // 原生统计只合并 global，不能把按账号、Key 等重复投影的同一请求再次相加
        // 贡献身份包含本次备份 ID，恢复后再备份时与上一次恢复的基线互不覆盖
        let mut stream = self.transaction.copy_out_raw(r#"COPY (
            WITH native AS MATERIALIZED (
                SELECT bucket_start, counts, costs FROM request_analytics_1h WHERE collection='global'
            ), counters AS (
                SELECT bucket_start, c.key, sum(c.value::numeric) AS value
                FROM native CROSS JOIN LATERAL jsonb_each_text(counts) c
                WHERE c.key IN ('request_count','success_count','failure_count','cancelled_count','incomplete_count',
                    'completed_request_count','input_tokens','output_tokens','cached_input_tokens','cache_write_input_tokens',
                    'reasoning_output_tokens','image_input_tokens','image_output_tokens','total_tokens')
                GROUP BY bucket_start,c.key
            ), amounts AS (
                SELECT bucket_start, c.key, sum(c.value::numeric)::text AS value
                FROM native CROSS JOIN LATERAL jsonb_each_text(costs) c
                WHERE c.key NOT IN ('standard_usd','no_cache_usd') GROUP BY bucket_start,c.key
            ), totals AS (
                SELECT bucket_start, jsonb_object_agg(key,value) AS counts FROM counters GROUP BY bucket_start
            ), costs AS (
                SELECT bucket_start, jsonb_object_agg(key,value) AS costs FROM amounts GROUP BY bucket_start
            )
            SELECT bucket_start, 'usage_contribution', ARRAY['','','','','','','backup:'||current_setting('cpr.backup_id')],
                '',0,0,counts,coalesce(costs.costs,'{}'::jsonb),'{}'::jsonb
            FROM totals LEFT JOIN costs USING(bucket_start)
            UNION ALL
            SELECT bucket_start,collection,dimensions,outcome,schema_version,histogram_version,counts,costs,distributions
            FROM request_analytics_1h WHERE collection='usage_contribution'
        ) TO STDOUT"#).await.map_err(|_| pg_dump_failed("无法导出用量汇总"))?;
        while let Some(bytes) = stream
            .try_next()
            .await
            .map_err(|_| pg_dump_failed("读取用量汇总失败"))?
        {
            writer.write_all(&bytes).await.map_err(write_failed)?;
        }
        drop(stream);
        let footer = format!(
            "\\.\nINSERT INTO {schema}.request_analytics_build_state (singleton) VALUES (true);\n\
             UPDATE {schema}.client_api_key_budget_windows SET charge_history_since = statement_timestamp();\n",
            schema = self.schema
        );
        writer
            .write_all(footer.as_bytes())
            .await
            .map_err(write_failed)?;
        self.transaction
            .commit()
            .await
            .map_err(|_| pg_dump_failed("关闭备份快照失败"))
    }
}
