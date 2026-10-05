//! 后台导入任务的有界请求与安全结果投影

use axum::extract::DefaultBodyLimit;
use gateway_admin::model::import_tasks::{
    ImportTaskDetail, ImportTaskInput, ImportTaskSummary, MAX_IMPORT_TASK_ITEMS, SubmitImportTask,
};
use sha2::{Digest as _, Sha256};

use super::*;
use crate::auth::SessionState;

pub(super) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/api/admin/accounts/import-tasks",
            get(list::<S>).post(submit::<S>),
        )
        .route("/api/admin/accounts/import-tasks/detail", get(detail::<S>))
        .route("/api/admin/accounts/import-tasks/stop", post(stop::<S>))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitRequest {
    submission_id: String,
    items: Vec<AccountImportRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskId {
    task_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryView {
    task_id: String,
    created_at: String,
    created_at_display: String,
    finished_at: Option<String>,
    finished_at_display: Option<String>,
    stop_requested: bool,
    total: usize,
    counts: CountsView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CountsView {
    pending: usize,
    running: usize,
    succeeded: usize,
    failed: usize,
    unknown: usize,
    skipped: usize,
    imported_accounts: usize,
}

impl From<(ImportTaskSummary, crate::time::TimePresenter)> for SummaryView {
    fn from((task, time): (ImportTaskSummary, crate::time::TimePresenter)) -> Self {
        let counts = task.counts;
        Self {
            task_id: task.task_id.to_string(),
            created_at_display: time.label(task.created_at, "%m-%d %H:%M:%S"),
            created_at: task.created_at.to_rfc3339(),
            finished_at_display: task
                .finished_at
                .map(|value| time.label(value, "%m-%d %H:%M:%S")),
            finished_at: task.finished_at.map(|at| at.to_rfc3339()),
            stop_requested: task.stop_requested,
            total: task.total,
            counts: CountsView {
                pending: counts.pending,
                running: counts.running,
                succeeded: counts.succeeded,
                failed: counts.failed,
                unknown: counts.unknown,
                skipped: counts.skipped,
                imported_accounts: counts.imported_accounts,
            },
        }
    }
}

#[derive(Serialize)]
struct DetailView {
    #[serde(flatten)]
    summary: SummaryView,
    items: Vec<ItemView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemView {
    index: usize,
    provider: String,
    status: &'static str,
    account_ids: Vec<String>,
    message: Option<String>,
}

impl From<(ImportTaskDetail, crate::time::TimePresenter)> for DetailView {
    fn from((task, time): (ImportTaskDetail, crate::time::TimePresenter)) -> Self {
        Self {
            summary: SummaryView::from((task.summary, time)),
            items: task
                .items
                .into_iter()
                .map(|item| ItemView {
                    index: item.index,
                    provider: item.provider.to_string(),
                    status: item.status.as_str(),
                    account_ids: item
                        .account_ids
                        .into_iter()
                        .map(|id| id.to_string())
                        .collect(),
                    message: item.message,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct ListView {
    items: Vec<SummaryView>,
}

async fn submit<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<SubmitRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let submission_id = Uuid::parse_str(&request.submission_id)
        .map_err(|_| map_wire_error(WireValidationError::new("submissionId")))?;
    if submission_id.is_nil() {
        return Err(map_wire_error(WireValidationError::new("submissionId")));
    }
    if request.items.is_empty() || request.items.len() > MAX_IMPORT_TASK_ITEMS {
        return Err(map_wire_error(WireValidationError::new("items")));
    }
    let encoded = serde_json::to_vec(&request.items)
        .map_err(|_| map_wire_error(WireValidationError::new("items")))?;
    let fingerprint = Sha256::digest(&encoded).into();
    drop(encoded);
    let context = auth.context().mutation_context();
    let items = request
        .items
        .into_iter()
        .map(|item| {
            let (provider, command) = item.into_command(context.clone())?;
            Ok(ImportTaskInput { provider, command })
        })
        .collect::<Result<Vec<_>, WireValidationError>>()
        .map_err(map_wire_error)?;
    let task = state
        .admin_services()
        .import_tasks()
        .submit(SubmitImportTask {
            submission_id,
            fingerprint,
            context,
            items,
        })
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::ACCEPTED,
        AdminEnvelope::ok(SummaryView::from((task, time))),
    ))
}

async fn list<S>(auth: AdminAuth, State(state): State<S>) -> impl IntoResponse
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let items = state
        .admin_services()
        .import_tasks()
        .list(&auth.context().mutation_context())
        .into_iter()
        .map(|value| SummaryView::from((value, time)))
        .collect();
    AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(ListView { items }))
}

async fn detail<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(query): AdminQuery<TaskId>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let task = state
        .admin_services()
        .import_tasks()
        .detail(
            &auth.context().mutation_context(),
            parse_task_id(&query.task_id)?,
        )
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DetailView::from((task, time))),
    ))
}

async fn stop<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(request): AdminJson<TaskId>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    let task = state
        .admin_services()
        .import_tasks()
        .stop(
            &auth.context().mutation_context(),
            parse_task_id(&request.task_id)?,
        )
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DetailView::from((task, time))),
    ))
}

fn parse_task_id(value: &str) -> Result<Uuid, AdminError> {
    Uuid::parse_str(value).map_err(|_| map_wire_error(WireValidationError::new("taskId")))
}
