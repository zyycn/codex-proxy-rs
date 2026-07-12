use std::{convert::Infallible, pin::Pin};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    fleet::{account::AccountStatus, store::StoredAccount},
    models::{service::ModelRefreshPlanAccount, types::CodexModelInfo},
    upstream::openai::{
        protocol::{
            responses::{CodexResponsesRequest, ResponsesSseFailure},
            sse::{SseEvent, encode_sse_event, parse_sse_events},
        },
        transport::{
            CodexClientError, CodexRequestContext, is_banned_auth_signal, is_banned_upstream_error,
        },
    },
};

use super::{AccountManageService, types::AccountManageError};

pub(super) type AccountTestStream = Pin<Box<dyn Stream<Item = Result<Bytes, Infallible>> + Send>>;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountModelOption {
    pub id: String,
    pub label: String,
}

struct AccountTestOutcome {
    event: Value,
    status: Option<AccountStatus>,
}

impl AccountTestOutcome {
    fn success() -> Self {
        Self {
            event: json!({ "type": "test_complete", "success": true }),
            status: Some(AccountStatus::Active),
        }
    }

    fn error(error: impl Into<String>, status: Option<AccountStatus>) -> Self {
        Self {
            event: json!({ "type": "error", "error": error.into() }),
            status,
        }
    }

    fn attach_account_status(&mut self, status: AccountStatus) {
        let Some(event) = self.event.as_object_mut() else {
            return;
        };
        event.insert(
            "accountStatus".to_string(),
            Value::String(status.as_str().to_string()),
        );
    }
}

impl AccountManageService {
    pub async fn account_models(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountModelOption>, AccountManageError> {
        let account = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AccountManageError::Inspect)?
            .ok_or(AccountManageError::NotFound)?;
        let plan_type = account_plan_type(&account);
        let mut models = self.models.catalog().await.models_for_plan(&plan_type);
        if models.is_empty() {
            self.refresh_account_plan_models(&account, &plan_type).await;
            models = self.models.catalog().await.models_for_plan(&plan_type);
        }
        let models = models.iter().map(account_model_option).collect::<Vec<_>>();
        if models.is_empty() {
            return Err(AccountManageError::NoModels);
        }
        Ok(models)
    }

    async fn refresh_account_plan_models(&self, account: &StoredAccount, plan_type: &str) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let plan_account = ModelRefreshPlanAccount {
            plan_type: plan_type.to_string(),
            access_token: account.access_token.expose_secret().to_string(),
            account_id: account.account_id.clone(),
            installation_id: self.account_pseudonymizer.installation_id(&account.id),
        };
        if let Err(error) = self
            .models
            .refresh_backend_models(&[plan_account], &request_id)
            .await
        {
            tracing::warn!(
                account_id = %account.id,
                plan_type,
                error = %error,
                "failed to refresh account plan models"
            );
        }
    }

    pub async fn test_connection_stream(
        &self,
        account_id: &str,
        model: String,
    ) -> Result<AccountTestStream, AccountManageError> {
        let account = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AccountManageError::Inspect)?
            .ok_or(AccountManageError::NotFound)?;

        let token = account.access_token.expose_secret().to_string();
        let upstream_account_id = account.account_id.clone();
        let cookie_header = self
            .cookies
            .cookie_header_for_request(&account.id, "chatgpt.com", "/codex/responses")
            .await
            .ok()
            .flatten();
        let installation_id = self.account_pseudonymizer.installation_id(&account.id);
        let codex = self.codex.clone();
        let service = self.clone();
        let stored_account_id = account.id.clone();
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = mpsc::channel::<Bytes>(16);

        tokio::spawn(async move {
            send_test_event(
                &tx,
                json!({
                    "type": "test_start",
                    "model": model.clone(),
                    "text": "正在连接 Codex Responses"
                }),
            )
            .await;

            let request = test_responses_request(model);
            send_test_event(
                &tx,
                json!({
                    "type": "request",
                    "payload": serde_json::to_value(&request).unwrap_or_else(|_| json!({}))
                }),
            )
            .await;

            let context = CodexRequestContext {
                access_token: &token,
                account_id: upstream_account_id.as_deref(),
                request_id: &request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: cookie_header.as_deref(),
                installation_id: Some(&installation_id),
                session_id: None,
                thread_id: None,
                prompt_cache_key: None,
                client_request_id: None,
                turn_id: None,
            };

            let mut outcome = match codex.create_response_stream(&request, context).await {
                Ok(response) => process_upstream_test_stream(response.body, &tx).await,
                Err(error) => AccountTestOutcome::error(
                    error.to_string(),
                    account_status_from_client_error(&error),
                ),
            };

            if let Some(status) = outcome.status
                && let Some(status) = service
                    .apply_connection_test_status(&stored_account_id, status)
                    .await
            {
                outcome.attach_account_status(status);
            }
            send_test_event(&tx, outcome.event).await;
        });

        let stream = futures::stream::unfold(rx, |mut rx| async {
            rx.recv()
                .await
                .map(|bytes| (Ok::<Bytes, Infallible>(bytes), rx))
        });
        Ok(Box::pin(stream))
    }

    async fn apply_connection_test_status(
        &self,
        account_id: &str,
        status: AccountStatus,
    ) -> Option<AccountStatus> {
        let current = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to inspect account after connection test"
                );
                return None;
            }
        };
        if current.status == AccountStatus::Disabled {
            return Some(AccountStatus::Disabled);
        }

        match self.store.set_status(account_id, status).await {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    status = %status,
                    error = %error,
                    "failed to persist account status after connection test"
                );
                return None;
            }
        }

        if matches!(status, AccountStatus::Expired | AccountStatus::Banned)
            && let Err(error) = self.store.set_next_refresh_at(account_id, None).await
        {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to clear token refresh schedule after connection test"
            );
        }
        self.sync_account_pool_best_effort(account_id, "connection test")
            .await;
        Some(status)
    }
}

fn account_model_option(model: &CodexModelInfo) -> AccountModelOption {
    AccountModelOption {
        id: model.id.clone(),
        label: model.display_name.clone(),
    }
}

fn account_plan_type(account: &StoredAccount) -> String {
    account
        .plan_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string()
}

fn test_responses_request(model: String) -> CodexResponsesRequest {
    let mut request = CodexResponsesRequest::new_http_sse(
        model,
        "You are checking whether this Codex account can answer. Reply with ok.",
        vec![json!({
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": "hi"
                }
            ]
        })],
    );
    request.set_stream(true);
    request.set_store(false);
    request.force_http_sse = true;
    request
}

async fn process_upstream_test_stream(
    mut body: crate::upstream::openai::transport::CodexBackendSseStream,
    tx: &mpsc::Sender<Bytes>,
) -> AccountTestOutcome {
    let mut buffer = String::new();

    while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(bytes) => bytes,
            Err(error) => {
                return AccountTestOutcome::error(
                    error.to_string(),
                    account_status_from_client_error(&error),
                );
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(frame) = take_sse_frame(&mut buffer) {
            if let Some(outcome) = process_sse_frame(tx, &frame).await {
                return outcome;
            }
        }
    }

    if !buffer.trim().is_empty()
        && let Some(outcome) = process_sse_frame(tx, &buffer).await
    {
        return outcome;
    }

    AccountTestOutcome::error("Stream ended before response.completed", None)
}

async fn process_sse_frame(tx: &mpsc::Sender<Bytes>, frame: &str) -> Option<AccountTestOutcome> {
    let events = match parse_sse_events(frame) {
        Ok(events) => events,
        Err(error) => {
            return Some(AccountTestOutcome::error(error.to_string(), None));
        }
    };

    for event in events {
        if let Some(outcome) = process_sse_event(tx, &event).await {
            return Some(outcome);
        }
    }
    None
}

async fn process_sse_event(
    tx: &mpsc::Sender<Bytes>,
    event: &SseEvent,
) -> Option<AccountTestOutcome> {
    let value: Value = match serde_json::from_str(&event.data) {
        Ok(value) => value,
        Err(_) => return None,
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                send_test_event(tx, json!({ "type": "content", "text": delta })).await;
            }
            None
        }
        Some("response.completed" | "response.done") => Some(AccountTestOutcome::success()),
        Some(event_name @ ("response.failed" | "error")) => {
            let failure = ResponsesSseFailure::from_event(event_name, &value);
            let status = sse_failure_account_status(&failure);
            Some(AccountTestOutcome::error(failure.message, status))
        }
        _ => None,
    }
}

fn account_status_from_client_error(error: &CodexClientError) -> Option<AccountStatus> {
    if is_banned_upstream_error(error) {
        Some(AccountStatus::Banned)
    } else {
        match error {
            CodexClientError::Upstream { status, .. } if status.as_u16() == 402 => {
                Some(AccountStatus::QuotaExhausted)
            }
            CodexClientError::Upstream { status, body, .. } if status.as_u16() == 401 => {
                Some(if is_banned_auth_signal(body) {
                    AccountStatus::Banned
                } else {
                    AccountStatus::Expired
                })
            }
            _ => None,
        }
    }
}

fn sse_failure_account_status(failure: &ResponsesSseFailure) -> Option<AccountStatus> {
    let code = failure
        .upstream_code
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = failure.message.to_ascii_lowercase();
    if matches!(code.as_str(), "quota_exceeded" | "insufficient_quota") || message.contains("quota")
    {
        return Some(AccountStatus::QuotaExhausted);
    }
    let authentication_failed = matches!(
        code.as_str(),
        "token_invalid"
            | "token_expired"
            | "token_revoked"
            | "account_deactivated"
            | "unauthorized"
            | "invalid_api_key"
    ) || message.contains("token revoked")
        || message.contains("token invalid")
        || message.contains("token expired");
    authentication_failed.then(|| {
        if is_banned_auth_signal(&failure.message) {
            AccountStatus::Banned
        } else {
            AccountStatus::Expired
        }
    })
}

fn take_sse_frame(buffer: &mut String) -> Option<String> {
    let index = buffer.find("\n\n").or_else(|| buffer.find("\r\n\r\n"))?;
    let delimiter_len = if buffer[index..].starts_with("\r\n\r\n") {
        4
    } else {
        2
    };
    let frame = buffer[..index + delimiter_len].to_string();
    buffer.drain(..index + delimiter_len);
    Some(frame)
}

async fn send_test_event(tx: &mpsc::Sender<Bytes>, event: Value) {
    let _ = tx
        .send(Bytes::from(encode_sse_event("", &event.to_string())))
        .await;
}
