//! 验证 OpenAI 模型目录的账号和版本隔离、缓存合并与失效

mod contract;
mod refresh;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{TimeZone as _, Utc};
use gateway_core::account::{ProviderAccount, ProviderAccountId, ProviderAccountStore};
use gateway_core::provider_ports::ProviderCatalogCachePort;
use gateway_core::routing::{
    ClientRoutingScope, FrozenAccountScope, ModelServiceTier, ProviderKind, RuntimeAccount,
    RuntimeAccountDirectory,
};
use provider_openai::OFFICIAL_CODEX_BASE_URL;
use provider_openai::credential::{
    CodexCredentialCatalogError, CodexCredentialCatalogService, CredentialRepositoryError,
    ImportCodexOAuthCredential,
};
use provider_openai::profile::{CodexWireProfile, CodexWireProfileState};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::support::{MemoryAccountStore, catalog_cache, profile, secret};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../../transport/fixtures/official_models_snapshot.json");

struct ReplacingCatalogResponder {
    calls: Arc<AtomicUsize>,
}

impl Respond for ReplacingCatalogResponder {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let body = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            OFFICIAL_FIXTURE.to_vec()
        } else {
            br#"{"models":[{"slug":"gpt-5.5","display_name":"GPT-5.5"}]}"#.to_vec()
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_raw(body, "application/json")
    }
}

/// 按调用顺序为连续读取 返回不同的目录正文
struct SequencedCatalogResponder {
    calls: Arc<AtomicUsize>,
    bodies: [&'static [u8]; 2],
}

impl Respond for SequencedCatalogResponder {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let index = self.calls.fetch_add(1, Ordering::SeqCst).min(1);
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_raw(self.bodies[index].to_vec(), "application/json")
    }
}

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        client_kind: provider_openai::profile::selection::ClientKind::Desktop,
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "catalog-contract".to_owned(),
        exact_user_agent: None,
        installation_seed: None,
        residency: None,
        account_routing_override: None,
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("fixture time"),
    })
}

fn service(store: &Arc<MemoryAccountStore>) -> CodexCredentialCatalogService {
    service_with_catalog_cache(store, OFFICIAL_CODEX_BASE_URL.to_owned(), catalog_cache())
}

fn service_with_catalog_cache(
    store: &Arc<MemoryAccountStore>,
    base_url: String,
    catalog_cache: Arc<dyn ProviderCatalogCachePort>,
) -> CodexCredentialCatalogService {
    CodexCredentialCatalogService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        base_url,
        catalog_cache,
        crate::support::runtime_policy(),
    )
}

async fn seed_account(store: &Arc<MemoryAccountStore>, account_id: &str) -> ProviderAccount {
    seed_account_with_plan(store, account_id, "pro").await
}

async fn seed_account_with_plan(
    store: &Arc<MemoryAccountStore>,
    account_id: &str,
    plan: &str,
) -> ProviderAccount {
    let mut verified_account = profile(&format!("chatgpt-{account_id}"));
    verified_account.plan_type = Some(plan.to_owned());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret(&format!("access-{account_id}")),
            verified_account,
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    store.account(account_id).expect("seeded account")
}

fn client_scope(accounts: &[ProviderAccount]) -> FrozenAccountScope {
    FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            accounts
                .iter()
                .map(|account| {
                    (
                        account.id().clone(),
                        RuntimeAccount::new(
                            ProviderKind::new("openai").expect("provider"),
                            Default::default(),
                        )
                        .with_model_access(account.model_access().clone()),
                    )
                })
                .collect(),
        )),
        ClientRoutingScope::all_accounts(),
    )
}
