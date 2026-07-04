use std::{convert::Infallible, pin::Pin};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::upstream::{
    accounts::{model::Account, store::StoredAccount},
    models::{info::CodexModelInfo, service::ModelRefreshPlanAccount},
    protocol::{
        responses::CodexResponsesRequest,
        sse::{encode_sse_event, parse_sse_events, SseEvent},
    },
    transport::CodexRequestContext,
};

use super::{contracts::AdminAccountError, AdminAccountService};

pub(super) type AccountTestStream = Pin<Box<dyn Stream<Item = Result<Bytes, Infallible>> + Send>>;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountModelOption {
    pub id: String,
    pub label: String,
}

impl AdminAccountService {
    pub async fn account_models(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountModelOption>, AdminAccountError> {
        let account = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AdminAccountError::Inspect)?
            .ok_or(AdminAccountError::NotFound)?;
        let plan_type = account_plan_type(&account);
        let mut models = self.models.catalog().await.models_for_plan(&plan_type);
        if models.is_empty() {
            self.refresh_account_plan_models(&account, &plan_type).await;
            models = self.models.catalog().await.models_for_plan(&plan_type);
        }
        let models = models.iter().map(account_model_option).collect::<Vec<_>>();
        if models.is_empty() {
            return Err(AdminAccountError::NoModels);
        }
        Ok(models)
    }

    async fn refresh_account_plan_models(&self, account: &StoredAccount, plan_type: &str) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let plan_account = ModelRefreshPlanAccount {
            plan_type: plan_type.to_string(),
            account: stored_account_to_model_refresh_account(account.clone()),
        };
        if let Err(error) = self
            .models
            .refresh_backend_models_with_installation_id(
                &[plan_account],
                &request_id,
                self.installation_id.as_deref(),
            )
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
    ) -> Result<AccountTestStream, AdminAccountError> {
        let account = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AdminAccountError::Inspect)?
            .ok_or(AdminAccountError::NotFound)?;

        let token = account.access_token.expose_secret().to_string();
        let upstream_account_id = account.account_id.clone();
        let cookie_header = self
            .cookies
            .cookie_header_for_request(&account.id, "chatgpt.com", "/codex/responses")
            .await
            .ok()
            .flatten();
        let installation_id = self.installation_id.clone();
        let codex = self.codex.clone();
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
                installation_id: installation_id.as_deref(),
                session_id: None,
            };

            match codex.create_response_stream(&request, context).await {
                Ok(response) => process_upstream_test_stream(response.body, tx).await,
                Err(error) => {
                    send_test_event(
                        &tx,
                        json!({
                            "type": "error",
                            "error": error.to_string()
                        }),
                    )
                    .await;
                }
            }
        });

        let stream = futures::stream::unfold(rx, |mut rx| async {
            rx.recv()
                .await
                .map(|bytes| (Ok::<Bytes, Infallible>(bytes), rx))
        });
        Ok(Box::pin(stream))
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

fn stored_account_to_model_refresh_account(stored: StoredAccount) -> Account {
    Account {
        id: stored.id,
        email: stored.email,
        account_id: stored.account_id,
        user_id: stored.user_id,
        label: stored.label,
        plan_type: stored.plan_type,
        access_token: stored.access_token.expose_secret().to_string(),
        refresh_token: stored
            .refresh_token
            .map(|token| token.expose_secret().to_string()),
        access_token_expires_at: stored.access_token_expires_at,
        next_refresh_at: stored.next_refresh_at,
        status: stored.status,
        quota_limit_reached: false,
        quota_verify_required: false,
        quota_cooldown_until: None,
        cloudflare_cooldown_until: None,
        request_count: 0,
        empty_response_count: 0,
        image_input_tokens: 0,
        image_output_tokens: 0,
        image_request_count: 0,
        image_request_failed_count: 0,
        window_request_count: 0,
        window_input_tokens: 0,
        window_output_tokens: 0,
        window_cached_tokens: 0,
        window_image_input_tokens: 0,
        window_image_output_tokens: 0,
        window_image_request_count: 0,
        window_image_request_failed_count: 0,
        window_started_at: None,
        window_reset_at: None,
        limit_window_seconds: None,
        added_at: stored.added_at,
        last_used_at: None,
    }
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
    request.force_http_sse = true;
    request
}

async fn process_upstream_test_stream(
    mut body: crate::upstream::transport::CodexBackendSseStream,
    tx: mpsc::Sender<Bytes>,
) {
    let mut buffer = String::new();

    while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(bytes) => bytes,
            Err(error) => {
                send_test_event(&tx, json!({ "type": "error", "error": error.to_string() })).await;
                return;
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(frame) = take_sse_frame(&mut buffer) {
            if process_sse_frame(&tx, &frame).await {
                return;
            }
        }
    }

    if !buffer.trim().is_empty() && process_sse_frame(&tx, &buffer).await {
        return;
    }

    send_test_event(
        &tx,
        json!({
            "type": "error",
            "error": "Stream ended before response.completed"
        }),
    )
    .await;
}

async fn process_sse_frame(tx: &mpsc::Sender<Bytes>, frame: &str) -> bool {
    let events = match parse_sse_events(frame) {
        Ok(events) => events,
        Err(error) => {
            send_test_event(
                tx,
                json!({
                    "type": "error",
                    "error": error.to_string()
                }),
            )
            .await;
            return true;
        }
    };

    for event in events {
        if process_sse_event(tx, &event).await {
            return true;
        }
    }
    false
}

async fn process_sse_event(tx: &mpsc::Sender<Bytes>, event: &SseEvent) -> bool {
    let value: Value = match serde_json::from_str(&event.data) {
        Ok(value) => value,
        Err(_) => return false,
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    send_test_event(tx, json!({ "type": "content", "text": delta })).await;
                }
            }
            false
        }
        Some("response.completed" | "response.done") => {
            send_test_event(tx, json!({ "type": "test_complete", "success": true })).await;
            true
        }
        Some("response.failed") => {
            send_test_event(
                tx,
                json!({
                    "type": "error",
                    "error": response_failure_message(&value)
                }),
            )
            .await;
            true
        }
        Some("error") => {
            send_test_event(
                tx,
                json!({
                    "type": "error",
                    "error": error_event_message(&value)
                }),
            )
            .await;
            true
        }
        _ => false,
    }
}

fn response_failure_message(value: &Value) -> String {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("OpenAI response failed")
        .to_string()
}

fn error_event_message(value: &Value) -> String {
    value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("Unknown upstream error")
        .to_string()
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
