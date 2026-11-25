//! 诊断文件的 HTTP 下载与受控数据投影，普通详情响应不携带时间线

use axum::{extract::State, http::header, response::IntoResponse};
use gateway_admin::model::{
    diagnostics::{DiagnosticBundle, DiagnosticTarget},
    observability::OpsError,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    AdminAuth, AdminError, AdminQuery, map_service_error, ops_error_view, usage_detail_view,
};
use crate::{SessionState, time::TimePresenter};

mod archive;
mod privacy;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ExportQuery {
    request_id: Option<String>,
    error_id: Option<String>,
}

pub(super) async fn download<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<ExportQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let target = match (query.request_id, query.error_id) {
        (Some(id), None) => DiagnosticTarget::Request(id),
        (None, Some(id)) => DiagnosticTarget::Operation(id),
        _ => {
            return Err(map_service_error(
                gateway_admin::model::AdminError::invalid("请指定一个请求 ID 或错误记录 ID"),
            ));
        }
    };
    let bundle = state
        .admin_services()
        .observability()
        .export_diagnostics(target)
        .await
        .map_err(map_service_error)?;
    let time = TimePresenter::new(state.admin_services().timezone());
    let id = match &bundle.target {
        DiagnosticTarget::Request(id) | DiagnosticTarget::Operation(id) => id,
    };
    let suffix = id
        .chars()
        .take(128)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let disposition = format!("attachment; filename=cpr-diagnostics-{suffix}.zip");
    let document = privacy::redact(document(bundle, time));
    let body = tokio::task::spawn_blocking(move || archive::build(&document))
        .await
        .map_err(archive_error)?
        .map_err(archive_error)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "private, no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        body,
    ))
}

fn archive_error(source: impl Into<gateway_core::error::ErrorSource>) -> AdminError {
    map_service_error(
        gateway_admin::model::AdminError::internal("诊断文件生成失败").with_source(source),
    )
}

fn document(bundle: DiagnosticBundle, time: TimePresenter) -> Value {
    let target = match &bundle.target {
        DiagnosticTarget::Request(id) => json!({"requestId": id}),
        DiagnosticTarget::Operation(id) => json!({"errorId": id}),
    };
    let requests = bundle.requests.into_iter().map(|record| {
        let evidence = record.detail.map(|mut detail| {
            let trace = detail.trace.take();
            let related = std::mem::take(&mut detail.related_requests);
            let configuration = json!({
                "revision": detail.request.config_revision,
                "routingScope": detail.request.routing_scope,
                "routingGroupIds": detail.request.routing_group_refs,
                "deadlineAt": detail.request.deadline_at,
            });
            // 页面 DTO 没有展示的尝试原因与等待事实仍属于导出证据
            let attempt_details = detail.attempts.iter().map(|attempt| json!({
                "id": attempt.id, "attemptIndex": attempt.attempt_index,
                "component": attempt.component, "operation": attempt.operation,
                "message": attempt.message, "retryAfterMs": attempt.retry_after_ms,
            })).collect::<Vec<_>>();
            json!({
                "detail": usage_detail_view(detail, time), "trace": trace,
                "relatedRequests": related, "incidentConfiguration": configuration,
                "attemptDetails": attempt_details,
            })
        });
        json!({
            "requestId": record.request_id,
            "evidence": evidence,
            "errors": record.errors.into_iter().map(|error| export_error(error, time)).collect::<Vec<_>>(),
        })
    }).collect::<Vec<_>>();
    json!({
        "schemaVersion": 5,
        "exportTruncated": false,
        "source": "gateway.diagnostics",
        "exportPolicy": "support_credentials_redacted",
        "target": target,
        "collectedAt": bundle.collected_at,
        "environment": {
            "scope": "export_time", "observedAt": bundle.collected_at,
            "gateway": bundle.environment.map(|version| json!({
                "version": version.version, "gitSha": version.git_sha,
                "buildTime": version.build_time, "deploymentMode": version.deployment_mode,
            })),
        },
        "error": bundle.error.map(|error| export_error(error, time)),
        "requests": requests,
        "availability": {"gaps": bundle.gaps.into_iter().map(|gap| json!({"section": gap.section, "reason": gap.reason})).collect::<Vec<_>>()},
        "omitted": ["credentials_and_authentication", "account_contact_details", "uncollected_wire_payloads", "unrelated_requests_and_system_secrets"],
        "notes": [
            "诊断包包含错误原因链、上游错误与业务上下文，用于故障分析",
            "认证字段、账号联系信息与可识别的内嵌凭据已脱敏，任意业务内容不属于自动识别范围",
            "environment 为导出时环境，incidentConfiguration 与 request.routed 为事发配置事实，当前版本不能代替事发版本",
            "缺失、截断、淘汰与读取失败见 availability.gaps，没有记录不表示没有发生",
            "attempts 为异步 best-effort 投影，firstTokenMs 为空不证明没有输出",
            "未采集的请求正文、过期记录、上游内部状态与退出前未落盘信息无法由导出补回"
        ],
    })
}

// 复用错误字段投影，原始详情直接进入脱敏遍历，避免先为页面缩进再解析一遍
fn export_error(mut error: OpsError, time: TimePresenter) -> Value {
    let details = error.error_details.take();
    let mut value = json!(ops_error_view(error, time));
    value["errorDetails"] = json!(details);
    value
}
