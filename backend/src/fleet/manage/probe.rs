use std::pin::Pin;

use futures::{Stream, StreamExt};
use secrecy::ExposeSecret;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    fleet::{
        account::AccountStatus,
        account_failure::classify_account_failure,
        account_gateway::{
            AccountGatewayError, AccountProbeEvent, AccountProbeRequest, AccountProbeSession,
            AccountUpstreamContext,
        },
        store::StoredAccount,
    },
    models::{
        service::{ModelRefreshPlanAccount, ModelRefreshResult},
        types::CodexModelInfo,
    },
};

use super::{AccountManageService, types::AccountManageError};

pub type AccountTestStream = Pin<Box<dyn Stream<Item = AccountTestEvent> + Send>>;

#[derive(Debug, Clone)]
pub enum AccountTestEvent {
    Started {
        model: String,
    },
    Request {
        payload: Value,
    },
    Content {
        text: String,
    },
    Complete {
        account_status: Option<AccountStatus>,
    },
    Error {
        error: String,
        account_status: Option<AccountStatus>,
    },
}

#[derive(Debug, Clone)]
pub struct AccountModelOption {
    pub id: String,
    pub label: String,
}

struct AccountTestOutcome {
    error: Option<String>,
    status: Option<AccountStatus>,
}

impl AccountTestOutcome {
    fn success() -> Self {
        Self {
            error: None,
            status: Some(AccountStatus::Active),
        }
    }

    fn error(error: impl Into<String>, status: Option<AccountStatus>) -> Self {
        Self {
            error: Some(error.into()),
            status,
        }
    }

    fn into_event(self) -> AccountTestEvent {
        match self.error {
            Some(error) => AccountTestEvent::Error {
                error,
                account_status: self.status,
            },
            None => AccountTestEvent::Complete {
                account_status: self.status,
            },
        }
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
        let models = self.models.catalog().await.models_for_plan(&plan_type);
        let models = models.iter().map(account_model_option).collect::<Vec<_>>();
        if models.is_empty() {
            return Err(AccountManageError::NoModels);
        }
        Ok(models)
    }

    pub async fn refresh_account_models(
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
        self.refresh_account_plan_models(&account, &plan_type)
            .await?;
        let models = self.models.catalog().await.models_for_plan(&plan_type);
        let models = models.iter().map(account_model_option).collect::<Vec<_>>();
        if models.is_empty() {
            return Err(AccountManageError::NoModels);
        }
        Ok(models)
    }

    async fn refresh_account_plan_models(
        &self,
        account: &StoredAccount,
        plan_type: &str,
    ) -> Result<ModelRefreshResult, AccountManageError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let plan_account = ModelRefreshPlanAccount {
            plan_type: plan_type.to_string(),
            access_token: account.access_token.expose_secret().to_string(),
            account_id: account.account_id.clone(),
            installation_id: self.account_pseudonymizer.installation_id(&account.id),
        };
        let result = self
            .models
            .refresh_selected_plan_models(&[plan_account], &request_id)
            .await
            .map_err(|error| AccountManageError::RefreshModels(error.to_string()))?;
        let routing = self
            .models
            .model_plan_routing()
            .await
            .map_err(|error| AccountManageError::RefreshModels(error.to_string()))?;
        self.account_pool
            .apply_model_plan_routing(routing.allowlist, routing.fetched_plan_types)
            .await;
        Ok(result)
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
        let context = AccountUpstreamContext {
            access_token: account.access_token.clone(),
            account_id: account.account_id.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
            cookie_header: self
                .cookies
                .cookie_header_for_request(&account.id, "chatgpt.com", "/codex/responses")
                .await
                .ok()
                .flatten(),
            installation_id: Some(self.account_pseudonymizer.installation_id(&account.id)),
        };
        let upstream = self.upstream.clone();
        let service = self.clone();
        let stored_account_id = account.id;
        let (tx, rx) = mpsc::channel(16);

        tokio::spawn(async move {
            send_test_event(
                &tx,
                AccountTestEvent::Started {
                    model: model.clone(),
                },
            )
            .await;
            let request = AccountProbeRequest {
                model,
                instructions:
                    "You are checking whether this Codex account can answer. Reply with ok."
                        .to_string(),
                input_text: "hi".to_string(),
            };
            let mut outcome = match upstream.probe_response(context, request).await {
                Ok(session) => process_probe_session(session, &tx).await,
                Err(error) => AccountTestOutcome::error(
                    error.to_string(),
                    account_status_from_gateway_error(&error),
                ),
            };
            if let Some(status) = outcome.status {
                outcome.status = service
                    .apply_connection_test_status(&stored_account_id, status)
                    .await;
            }
            send_test_event(&tx, outcome.into_event()).await;
        });

        let stream = futures::stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|event| (event, rx))
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
                    "Failed to inspect account after connection test"
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
                    "Failed to persist account status after connection test"
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
                "Failed to clear token refresh schedule after connection test"
            );
        }
        self.sync_account_pool_best_effort(account_id, "connection test")
            .await;
        Some(status)
    }
}

async fn process_probe_session(
    mut session: AccountProbeSession,
    tx: &mpsc::Sender<AccountTestEvent>,
) -> AccountTestOutcome {
    send_test_event(
        tx,
        AccountTestEvent::Request {
            payload: session.request_payload,
        },
    )
    .await;
    while let Some(event) = session.events.next().await {
        match event {
            AccountProbeEvent::Content(text) => {
                send_test_event(tx, AccountTestEvent::Content { text }).await;
            }
            AccountProbeEvent::Complete => return AccountTestOutcome::success(),
            AccountProbeEvent::Failed(error) => {
                return AccountTestOutcome::error(
                    error.to_string(),
                    account_status_from_gateway_error(&error),
                );
            }
        }
    }
    AccountTestOutcome::error("Stream ended before response.completed", None)
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

fn account_status_from_gateway_error(error: &AccountGatewayError) -> Option<AccountStatus> {
    classify_account_failure(error.failure()?)?.account_status()
}

async fn send_test_event(tx: &mpsc::Sender<AccountTestEvent>, event: AccountTestEvent) {
    let _ = tx.send(event).await;
}
