//! 按故障目标收集诊断证据，补充读取失败不丢弃已取得的记录

use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};

use tokio::time::{Instant, timeout_at};

mod availability;

use super::DefaultObservabilityService;
use crate::model::{
    AdminError, AdminErrorKind,
    diagnostics::{DiagnosticBundle, DiagnosticGap, DiagnosticRequest, DiagnosticTarget},
    observability::OpsError,
};
use crate::use_case::map_store_error;

const MAX_REQUESTS: usize = 8;
const MAX_ERRORS: u16 = 128;

impl DefaultObservabilityService {
    pub(super) async fn collect_diagnostics(
        &self,
        target: DiagnosticTarget,
    ) -> Result<DiagnosticBundle, AdminError> {
        if target.id().trim().is_empty() || target.id().len() > 256 {
            return Err(AdminError::invalid("诊断目标 ID 不合法"));
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let (collected, environment) = tokio::join!(
            self.collect_records(target, deadline),
            timeout_at(deadline, self.system.version()),
        );
        let mut bundle = collected?;
        match environment {
            Ok(Ok(version)) => bundle.environment = Some(version),
            _ => gap(&mut bundle, "environment", "collection_failed"),
        }
        Ok(bundle)
    }

    async fn collect_records(
        &self,
        target: DiagnosticTarget,
        deadline: Instant,
    ) -> Result<DiagnosticBundle, AdminError> {
        let mut bundle = DiagnosticBundle {
            target,
            collected_at: chrono::Utc::now(),
            environment: None,
            error: None,
            requests: Vec::new(),
            gaps: Vec::new(),
        };
        let mut remaining_errors = MAX_ERRORS;
        let first_id = match &bundle.target {
            DiagnosticTarget::Request(id) => Some(id.clone()),
            DiagnosticTarget::Operation(_) => {
                let mut errors = self.export_errors(&bundle.target, 1, deadline).await?;
                let error = errors
                    .pop()
                    .ok_or_else(|| AdminError::not_found("错误记录不存在或已过期"))?;
                let id = error.request_id.clone();
                bundle.error = Some(error);
                remaining_errors -= 1;
                id
            }
        };
        let mut pending = VecDeque::from_iter(first_id);
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if bundle.requests.len() >= MAX_REQUESTS {
                gap(&mut bundle, "requests", "related_request_limit");
                break;
            }
            let section = format!("requests.{id}");
            let target = DiagnosticTarget::Request(id.clone());
            let selected = bundle
                .error
                .as_ref()
                .filter(|error| error.request_id.as_deref() == Some(id.as_str()));
            // 事件导出已经持有选中记录，额外读取一个槽位用于去重后判断是否超限
            let read_limit = remaining_errors + 1 + u16::from(selected.is_some());
            let (detail, errors) = tokio::join!(
                timeout_at(deadline, self.store.usage_record_detail(&id)),
                self.export_errors(&target, read_limit, deadline),
            );
            let (detail, detail_error) = match detail {
                Ok(Ok(detail)) => (Some(detail), None),
                Ok(Err(error)) => (None, Some(map_store_error(error, "diagnostic request"))),
                Err(_) => (
                    None,
                    Some(AdminError::new(AdminErrorKind::Unavailable, "诊断采集超时")),
                ),
            };
            let mut errors = match errors {
                Ok(mut errors) => {
                    if let Some(selected) = selected {
                        errors.retain(|error| {
                            error.source != selected.source || error.event_id != selected.event_id
                        });
                    }
                    errors
                }
                Err(error) => {
                    if detail.is_none() && bundle.requests.is_empty() && bundle.error.is_none() {
                        return Err(error);
                    }
                    gap(
                        &mut bundle,
                        &format!("{section}.errors"),
                        "collection_failed",
                    );
                    Vec::new()
                }
            };
            if detail.is_none()
                && errors.is_empty()
                && bundle.requests.is_empty()
                && bundle.error.is_none()
            {
                return Err(
                    detail_error.unwrap_or_else(|| AdminError::not_found("请求记录不存在或已过期"))
                );
            }
            if errors.len() > usize::from(remaining_errors) {
                gap(
                    &mut bundle,
                    &format!("{section}.errors"),
                    "error_record_limit",
                );
                errors.truncate(usize::from(remaining_errors));
            }
            remaining_errors =
                remaining_errors.saturating_sub(u16::try_from(errors.len()).unwrap_or(MAX_ERRORS));
            if let Some(record) = &detail {
                for related in &record.related_requests {
                    if let Some(id) = related.get("requestId").and_then(serde_json::Value::as_str) {
                        pending.push_back(id.to_owned());
                    }
                }
                if record.request.outcome == crate::model::observability::RequestOutcome::Running {
                    gap(&mut bundle, &section, "request_not_finalized");
                }
                match &record.trace {
                    None => gap(
                        &mut bundle,
                        &format!("{section}.trace"),
                        "not_recorded_or_retained",
                    ),
                    Some(trace) => availability::trace_gaps(&mut bundle, &section, trace),
                }
                gap(
                    &mut bundle,
                    &format!("{section}.attempts"),
                    "best_effort_persistence",
                );
            } else {
                gap(&mut bundle, &section, "request_detail_unavailable");
            }
            bundle.requests.push(DiagnosticRequest {
                request_id: id,
                detail,
                errors,
            });
        }
        Ok(bundle)
    }

    async fn export_errors(
        &self,
        target: &DiagnosticTarget,
        limit: u16,
        deadline: Instant,
    ) -> Result<Vec<OpsError>, AdminError> {
        timeout_at(deadline, self.store.diagnostic_errors(target, limit))
            .await
            .map_err(|_| AdminError::new(AdminErrorKind::Unavailable, "诊断采集超时"))?
            .map_err(|error| map_store_error(error, "diagnostic errors"))
    }
}

fn gap(bundle: &mut DiagnosticBundle, section: &str, reason: &'static str) {
    if !bundle
        .gaps
        .iter()
        .any(|gap| gap.section == section && gap.reason == reason)
    {
        bundle.gaps.push(DiagnosticGap {
            section: section.to_owned(),
            reason,
        });
    }
}
