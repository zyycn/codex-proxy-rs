//! `model_requests` 完成用量事实的 PostgreSQL 语义。

use sqlx::{Postgres, QueryBuilder};

/// 只把已完整交付给客户端、且存在用量证据的成功响应投影为用量事实。
///
/// `model_requests` 同时承担执行审计：包括上游已发送、但尚未收到首个事件就断开的
/// WebSocket 请求。这些失败仍必须留给 Ops Errors 和调度健康度分析，不能混入用量、
/// 成本、账号使用次数或请求明细。
/// Provider 原生端点也会写入执行审计；standalone search 没有模型、Token 或费用事实，
/// 因此不属于用量记录。图片端点由请求意图证明其用量语义，不依赖文本模型字段。
/// 客户端 WebSocket 的每个 `response.create` 没有独立 HTTP 状态码；成功终态与下游
/// 提交边界已足以证明交付，连接握手的 101 不能冒充单次请求状态。
/// OpenAI Provider 将 `generate=false` 归类为 `prewarm`：它只准备连接与上下文，
/// 即使响应带有模型和输入 Token，也不是推理用量；原始审计和费用事实仍保留。
pub(crate) fn completed_usage_fact_predicate(alias: &str) -> String {
    let evidence = usage_evidence_predicate(alias);
    format!(
        "{alias}.outcome = 'succeeded'
         and {alias}.downstream_committed_at is not null
         and ({alias}.provider_kind is distinct from 'openai'
              or {alias}.request_kind is distinct from 'prewarm')
         and (({alias}.client_transport = 'websocket' and {alias}.client_status_code is null)
              or {alias}.client_status_code between 200 and 399)
         and ({evidence})"
    )
}

fn usage_evidence_predicate(alias: &str) -> String {
    format!(
        "{alias}.requested_model_id is not null
         or {alias}.upstream_model_id is not null
         or {alias}.image_generation_requested
         or {alias}.input_tokens is not null
         or {alias}.output_tokens is not null
         or {alias}.cached_tokens is not null
         or {alias}.cache_write_tokens is not null
         or {alias}.reasoning_tokens is not null
         or {alias}.image_input_tokens is not null
         or {alias}.image_output_tokens is not null
         or {alias}.total_tokens is not null
         or {alias}.cost_amount is not null"
    )
}

pub(crate) fn push_completed_usage_fact_filter(query: &mut QueryBuilder<Postgres>, alias: &str) {
    query.push(format!(" and {}", completed_usage_fact_predicate(alias)));
}

/// 排除已由后续成功请求恢复的会话续接中间失败。
///
/// 原始请求审计仍保留该行；默认业务指标只把最终成功链视为一次结果。
pub(crate) fn push_unrecovered_request_filter(query: &mut QueryBuilder<Postgres>, alias: &str) {
    query.push(format!(" and {alias}.recovered_at is null"));
}
