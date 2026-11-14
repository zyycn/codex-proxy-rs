//! 验证 OpenAI 模型目录的账号和版本隔离、缓存合并与失效

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
    CodexCredentialCatalogError, CodexCredentialCatalogService, ImportCodexOAuthCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::support::{MemoryAccountStore, catalog_cache, profile, secret};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("../../transport/fixtures/official_models_snapshot.json");

#[tokio::test]
async fn discovery_refresh_and_export_use_the_current_global_profile() {
    use gateway_core::account::OpaqueProviderData;
    use serde_json::json;

    let document =
        |value: serde_json::Value| OpaqueProviderData::new(value.as_object().unwrap().clone());
    let fixed = |version| {
        document(
            json!({"client":"cli","platform":"macos","versionMode":"fixed","codexVersion":version}),
        )
    };
    let store = Arc::new(MemoryAccountStore::default());
    let first = seed_account(&store, "acct_profile_a").await;
    let second = seed_account(&store, "acct_profile_b").await;
    let server = MockServer::start().await;
    for (version, originator, count) in [
        ("0.169.0", "codex_cli_rs", 2),
        ("0.170.0", "codex_cli_rs", 1),
        ("0.171.0", "catalog-test", 1),
    ] {
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .and(query_param("client_version", version))
            .and(header("version", version))
            .and(header("originator", originator))
            .and(move |request: &Request| request.headers.get("user-agent").and_then(|v| v.to_str().ok()).is_some_and(|ua| ua.starts_with(&format!("{originator}/{version}"))))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[{"slug":format!("model-{version}"),"display_name":"Native","future_field":true}]})))
            .expect(count).mount(&server).await;
    }
    let policy = Arc::new(crate::support::RequestProfileRuntimePolicy(
        std::sync::Mutex::new(fixed("0.169.0")),
    ));
    let baseline = wire_profile();
    let service = CodexCredentialCatalogService::new(
        store.repository(),
        baseline.clone(),
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
        catalog_cache(),
        policy.clone(),
    );
    service.refresh_catalogs().await.unwrap();
    for account in [&first, &second] {
        assert_eq!(
            service.cached_account_models(account).unwrap().unwrap(),
            ["model-0.169.0"]
        );
    }
    *policy.0.lock().unwrap() = fixed("0.170.0");
    assert_eq!(
        service
            .refresh_account_catalog(first.id())
            .await
            .unwrap()
            .models(),
        ["model-0.170.0"]
    );
    *policy.0.lock().unwrap() = document(
        json!({"mode":"custom","userAgent":"catalog-test/0.171.0","originator":"catalog-test","codexVersion":"0.171.0"}),
    );
    let (models, _) = service.account_catalog_documents(&first).await.unwrap();
    assert_eq!(models[0].request_model().as_str(), "model-0.171.0");
    assert_eq!(baseline.snapshot().codex_version, "0.144.0");

    *policy.0.lock().unwrap() = document(json!({"mode":"custom","userAgent":"invalid"}));
    assert!(matches!(
        service.refresh_account_catalog(first.id()).await,
        Err(CodexCredentialCatalogError::Profile(_))
    ));
    assert_eq!(
        service.cached_account_models(&first).unwrap().unwrap(),
        ["model-0.170.0"]
    );
}

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
        client_kind: provider_openai::transport::profile::selection::ClientKind::Desktop,
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

#[tokio::test]
async fn restored_catalog_is_account_scoped_and_keeps_original_objects() {
    let store = Arc::new(MemoryAccountStore::default());
    let first = seed_account(&store, "acct_client_a").await;
    let second = seed_account(&store, "acct_client_b").await;
    let denied = second.clone().with_model_access(
        gateway_core::account::AccountModelAccess::new(
            gateway_core::account::AccountModelAccessMode::Denylist,
            vec!["gpt-extra".into()],
        )
        .unwrap(),
    );
    let server = MockServer::start().await;
    for (account, text) in [
        ("acct_client_a", "a-original"),
        ("acct_client_b", "b-original"),
    ] {
        Mock::given(method("GET")).and(path("/codex/models"))
            .and(header("authorization", format!("Bearer access-{account}")))
            .and(header("chatgpt-account-id", format!("chatgpt-{account}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"models":[{
                "slug":"gpt-native", "display_name":"Native", "base_instructions":text,
                "future_field":{"null":null,"list":[1,true]}, "model_messages":{"new_template":text}
            }, {"slug": if account == "acct_client_b" {"gpt-extra"} else {"gpt-native-a"}, "display_name":"Additional"}]}))).expect(1).mount(&server).await;
    }
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    service.refresh_catalogs().await.unwrap();
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    service.restore().await.unwrap();
    for (accounts, text, expected) in [
        (
            vec![second.clone(), first.clone()],
            "a-original",
            vec!["gpt-native", "gpt-native-a", "gpt-extra"],
        ),
        (vec![second], "b-original", vec!["gpt-native", "gpt-extra"]),
        (
            vec![first.clone(), denied],
            "a-original",
            vec!["gpt-native", "gpt-native-a"],
        ),
        (
            vec![first],
            "a-original",
            vec!["gpt-native", "gpt-native-a"],
        ),
    ] {
        let models = service
            .client_model_catalog(&client_scope(&accounts))
            .await
            .expect("catalog");
        assert_eq!(
            models.iter().map(|m| m.model.as_str()).collect::<Vec<_>>(),
            expected
        );
        let document: serde_json::Value = serde_json::from_slice(match &models[0].content {
            gateway_core::routing::ProviderModelContent::Native(payload) => payload.body(),
            _ => panic!("expected native model"),
        })
        .expect("document");
        assert_eq!(
            document,
            serde_json::json!({
                "slug":"gpt-native", "display_name":"Native", "base_instructions":text,
                "future_field":{"null":null,"list":[1,true]}, "model_messages":{"new_template":text}
            })
        );
    }
    assert!(
        service
            .client_model_catalog(&client_scope(&[]))
            .await
            .is_err()
    );
    server.verify().await;
}

#[tokio::test]
async fn client_catalog_cache_does_not_survive_credential_rotation() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = seed_account(&store, "acct_client_rotate").await;
    let scope = client_scope(std::slice::from_ref(&account));
    let server = MockServer::start().await;
    for (token, text) in [
        ("access-acct_client_rotate", "old"),
        ("rotated-access", "new"),
    ] {
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .and(header("authorization", format!("Bearer {token}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"models":[{
                    "slug":"gpt-native", "display_name":"Native", "base_instructions":text
                }]})),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    service.refresh_catalogs().await.unwrap();
    let first = service.client_model_catalog(&scope).await.expect("old");
    store
        .repository()
        .rotate_refreshed_oauth_secret(&account, secret("rotated-access"), None, None)
        .await
        .expect("rotate");
    assert!(service.client_model_catalog(&scope).await.is_err());
    service.refresh_catalogs().await.unwrap();
    let second = service.client_model_catalog(&scope).await.expect("new");
    assert_ne!(first, second);
    server.verify().await;
}

#[tokio::test]
async fn failed_client_catalog_never_falls_back_to_an_out_of_scope_account_or_cached_profile() {
    let store = Arc::new(MemoryAccountStore::default());
    let allowed = seed_account(&store, "acct_client_allowed").await;
    seed_account(&store, "acct_client_outside").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .and(header("authorization", "Bearer access-acct_client_allowed"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    let scope = client_scope(&[allowed]);
    assert!(service.refresh_catalogs().await.is_err());
    for _ in 0..2 {
        assert!(service.client_model_catalog(&scope).await.is_err());
    }
    assert_eq!(server.received_requests().await.expect("requests").len(), 2);
    server.verify().await;
}

#[tokio::test]
async fn raw_instructions_only_change_advances_catalog_generation() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account(&store, "acct_raw_generation").await;
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/codex/models"))
        .respond_with(SequencedCatalogResponder {
            calls: Arc::new(AtomicUsize::new(0)),
            bodies: [
                br#"{"models":[{"slug":"gpt-native","display_name":"Native","model_messages":{"future":"old"}}]}"#,
                br#"{"models":[{"slug":"gpt-native","display_name":"Native","model_messages":{"future":"new"}}]}"#,
            ],
        }).expect(2).mount(&server).await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    service.refresh_catalogs().await.expect("initial");
    let generation = service.catalog_generation();
    service
        .observe_response_etag(&store.account("acct_raw_generation").unwrap(), "new")
        .unwrap();
    service.refresh_catalogs().await.expect("refresh");
    assert!(service.catalog_generation() > generation);
    server.verify().await;
}

#[test]
fn catalog_starts_without_a_snapshot_or_generation() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    assert!(service.cached().expect("cache read").is_none());
    assert_eq!(service.catalog_generation().get(), 0);
}

#[tokio::test]
async fn catalog_without_openai_accounts_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());

    let error = service(&store)
        .refresh_catalogs()
        .await
        .expect_err("empty account set");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::NoEligibleCredential
    ));
}

#[tokio::test]
async fn missing_account_refresh_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = ProviderAccountId::new("acct_missing").expect("account id");

    let error = service(&store)
        .refresh_account_catalog(&account)
        .await
        .expect_err("missing account");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::NoEligibleCredential
    ));
}

#[tokio::test]
async fn disabled_account_refresh_discovers_models_with_the_pinned_account() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = seed_account(&store, "acct_disabled_models").await;
    store
        .set_enabled(account.id(), false)
        .await
        .expect("disable account");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(
                    br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4"}]}"#.to_vec(),
                    "application/json",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let catalog = service
        .refresh_account_catalog(account.id())
        .await
        .expect("disabled account refresh still discovers models");

    assert_eq!(catalog.models(), ["gpt-5.4"]);
    server.verify().await;
}

#[tokio::test]
async fn refreshing_one_account_does_not_replace_a_peers_cached_catalog() {
    let store = Arc::new(MemoryAccountStore::default());
    let first = seed_account(&store, "acct_catalog_a").await;
    let second = seed_account(&store, "acct_catalog_b").await;
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ReplacingCatalogResponder {
            calls: Arc::clone(&calls),
        })
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let initial = service
        .cached_or_refresh_account_catalog(&first)
        .await
        .expect("cold cache fetch");
    let cached = service
        .cached_or_refresh_account_catalog(&first)
        .await
        .expect("account cache hit");
    let refreshed = service
        .refresh_account_catalog(second.id())
        .await
        .expect("manual refresh");
    let replaced = service
        .cached_or_refresh_account_catalog(&first)
        .await
        .expect("peer cache hit");

    assert_eq!(initial.models(), ["gpt-5.4"]);
    assert_eq!(cached.models(), ["gpt-5.4"]);
    assert_eq!(refreshed.models(), ["gpt-5.5"]);
    assert_eq!(replaced.models(), ["gpt-5.4"]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.verify().await;
}

#[tokio::test]
async fn service_tier_changes_should_replace_cached_metadata_and_advance_generation() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account(&store, "acct_service_tiers").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(SequencedCatalogResponder {
            calls: Arc::new(AtomicUsize::new(0)),
            bodies: [
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","service_tiers":[{"id":"priority","name":"Fast","description":"Priority processing."}]}]}"#,
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","service_tiers":[]}]}"#,
            ],
        })
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let initial = service.refresh_catalogs().await.expect("initial catalog");
    let generation = service.catalog_generation().get();
    let cached = service.refresh_catalogs().await.expect("cached catalog");
    assert_eq!(cached.models(), initial.models());
    assert_eq!(
        cached.models()[0].metadata().service_tiers(),
        [ModelServiceTier::new(
            "priority",
            "Fast",
            "Priority processing."
        )]
    );
    assert_eq!(service.catalog_generation().get(), generation);

    service
        .observe_response_etag(&store.account("acct_service_tiers").unwrap(), "new")
        .unwrap();
    let refreshed = service.refresh_catalogs().await.expect("updated catalog");
    assert!(refreshed.models()[0].metadata().service_tiers().is_empty());
    assert_eq!(service.catalog_generation().get(), generation + 1);
    assert_eq!(
        service
            .cached()
            .expect("cache read")
            .expect("cached snapshot")
            .models(),
        refreshed.models()
    );
}

#[tokio::test]
async fn failed_oauth_catalog_does_not_hide_healthy_peers_or_borrow_their_models() {
    let store = Arc::new(MemoryAccountStore::default());
    let failed = seed_account(&store, "acct_failed").await;
    let healthy = seed_account(&store, "acct_healthy").await;
    let server = MockServer::start().await;
    for (account, status) in [(&failed, 503), (&healthy, 200)] {
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .and(header(
                "chatgpt-account-id",
                format!("chatgpt-{}", account.id()),
            ))
            .respond_with(
                ResponseTemplate::new(status).set_body_raw(OFFICIAL_FIXTURE, "application/json"),
            )
            .expect(if status == 503 { 2 } else { 1 })
            .mount(&server)
            .await;
    }
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    service.refresh_catalogs().await.unwrap();
    let models = service
        .client_model_catalog(&client_scope(&[failed.clone(), healthy]))
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model.as_str(), "gpt-5.4");
    let error = service
        .refresh_account_catalog(failed.id())
        .await
        .expect_err("account refresh must not use a peer");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::Upstream { .. }
    ));
    server.verify().await;
}

#[tokio::test]
async fn cross_plan_presentation_drift_unions_on_the_first_seen_model() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account_with_plan(&store, "acct_union_plus", "plus").await;
    seed_account_with_plan(&store, "acct_union_pro", "pro").await;
    let server = MockServer::start().await;
    for (account, name) in [
        ("acct_union_plus", "GPT-5.4 Plus"),
        ("acct_union_pro", "GPT-5.4 Pro"),
    ] {
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .and(header("chatgpt-account-id", format!("chatgpt-{account}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models":[{"slug":"gpt-5.4","display_name":name,"supported_in_api":true}]
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let snapshot = service
        .refresh_catalogs()
        .await
        .expect("presentation drift must not wedge the union");

    assert_eq!(snapshot.models().len(), 1);
    assert_eq!(snapshot.models()[0].display_name(), "GPT-5.4 Plus");
    server.verify().await;
}

#[tokio::test]
async fn cross_plan_supported_in_api_drift_unions_on_the_first_seen_model() {
    let store = Arc::new(MemoryAccountStore::default());
    seed_account_with_plan(&store, "acct_conflict_plus", "plus").await;
    seed_account_with_plan(&store, "acct_conflict_pro", "pro").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(SequencedCatalogResponder {
            calls: Arc::new(AtomicUsize::new(0)),
            bodies: [
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","supported_in_api":true}]}"#,
                br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4","supported_in_api":false}]}"#,
            ],
        })
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let snapshot = service
        .refresh_catalogs()
        .await
        .expect("supported_in_api drift must not block catalog publication");

    assert_eq!(snapshot.models().len(), 1);
    server.verify().await;
}

#[tokio::test]
async fn account_entitlements_keep_the_discovering_account_across_plans() {
    let store = Arc::new(MemoryAccountStore::default());
    let first_free = seed_account_with_plan(&store, "acct_free_a", "free").await;
    let second_free = seed_account_with_plan(&store, "acct_free_b", "free").await;
    let k12 = seed_account_with_plan(&store, "acct_k12", "k12").await;
    let server = MockServer::start().await;
    for (account, extra) in [(&first_free, false), (&second_free, true), (&k12, true)] {
        let mut models = vec![serde_json::json!({"slug":"gpt-5.4","display_name":"GPT-5.4"})];
        if extra {
            models.push(serde_json::json!({"slug":"gpt-5.6-sol","display_name":"Sol"}));
        }
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .and(header(
                "chatgpt-account-id",
                format!("chatgpt-{}", account.id()),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"models":models})),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let snapshot = service
        .refresh_catalogs()
        .await
        .expect("mixed-plan catalog");

    assert_eq!(snapshot.models().len(), 2);
    assert_eq!(
        service
            .cached_account_models(&first_free)
            .expect("first free entitlement"),
        Some(vec!["gpt-5.4".to_owned()])
    );
    assert_eq!(
        service
            .cached_account_models(&second_free)
            .expect("second free entitlement"),
        Some(vec!["gpt-5.4".to_owned(), "gpt-5.6-sol".to_owned()])
    );
    assert_eq!(
        service.cached_account_models(&k12).expect("K12 catalog"),
        Some(vec!["gpt-5.4".to_owned(), "gpt-5.6-sol".to_owned()])
    );
    assert_eq!(
        snapshot.model_catalog_accounts()["gpt-5.6-sol"],
        std::collections::BTreeSet::from([second_free.id().clone(), k12.id().clone()])
    );
    let restarted = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    restarted.restore().await.unwrap();
    let restored = restarted.refresh_catalogs().await.unwrap();
    assert_eq!(
        restored.model_catalog_accounts(),
        snapshot.model_catalog_accounts()
    );
    server.verify().await;
}

#[tokio::test]
async fn invalidation_queues_background_refresh_without_an_upstream_etag() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);
    service
        .invalidate_accounts(&[ProviderAccountId::new("acct_first").unwrap()])
        .await
        .unwrap();
    service
        .invalidate_accounts(&[ProviderAccountId::new("acct_first").unwrap()])
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        service.wait_for_refresh(),
    )
    .await
    .expect("configuration invalidation must wake the catalog worker");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            service.wait_for_refresh(),
        )
        .await
        .is_err(),
        "duplicate invalidations should share one pending refresh"
    );
}

#[tokio::test]
async fn simultaneous_periodic_refreshes_share_the_completed_scan() {
    let store = Arc::new(MemoryAccountStore::default());
    for index in 0..64 {
        seed_account(&store, &format!("acct_refresh_{index:02}")).await;
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(OFFICIAL_FIXTURE.to_vec(), "application/json")
                .set_delay(std::time::Duration::from_millis(20)),
        )
        .expect(64)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    let (first, second, third) = tokio::join!(
        service.refresh_catalogs(),
        service.refresh_catalogs(),
        service.refresh_catalogs(),
    );
    let first = first.expect("first refresh");
    assert_eq!(first.models(), second.expect("shared refresh").models());
    assert_eq!(first.models(), third.expect("shared refresh").models());
    server.verify().await;
}

#[tokio::test]
async fn api_key_catalogs_are_isolated_and_join_oauth_without_claiming_native_metadata() {
    use provider_openai::credential::ResponsesTransport;
    let oauth = MockServer::start().await;
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    let oauth_account = seed_account(&store, "acct_oauth").await;
    store
        .seed_api_key("acct_first", first.uri(), ResponsesTransport::Http)
        .await;
    store
        .seed_api_key("acct_second", second.uri(), ResponsesTransport::Http)
        .await;
    let a = store.account("acct_first").unwrap();
    let b = store.account("acct_second").unwrap();
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"models":[{"slug":"oauth-model","display_name":"OAuth model"}]}),
        ))
        .mount(&oauth)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data":[{"id":"api-first"}]})),
        )
        .mount(&first)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data":[{"id":"api-second"}]})),
        )
        .mount(&second)
        .await;
    let service = service_with_catalog_cache(&store, oauth.uri(), catalog_cache());
    service.refresh_catalogs().await.unwrap();
    assert_eq!(
        service.cached_account_models(&a).unwrap().unwrap(),
        ["api-first"]
    );
    assert_eq!(
        service.cached_account_models(&b).unwrap().unwrap(),
        ["api-second"]
    );
    let combined = service
        .client_model_catalog(&client_scope(&[oauth_account, a.clone()]))
        .await
        .unwrap();
    assert_eq!(
        combined
            .iter()
            .map(|entry| entry.model.as_str())
            .collect::<Vec<_>>(),
        ["oauth-model", "api-first"]
    );
    assert!(matches!(
        combined[0].content,
        gateway_core::routing::ProviderModelContent::Native(_)
    ));
    assert!(matches!(
        combined[1].content,
        gateway_core::routing::ProviderModelContent::Adapted(_)
    ));
    first.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&first)
        .await;
    service
        .invalidate_accounts(&[ProviderAccountId::new("acct_first").unwrap()])
        .await
        .unwrap();
    service
        .refresh_catalogs()
        .await
        .expect("failed upstream must not hide other accounts");
    assert_eq!(
        service.cached_account_models(&b).unwrap().unwrap(),
        ["api-second"]
    );
}

#[tokio::test]
async fn api_key_background_catalog_preserves_negotiated_reasoning_levels_and_entitlements() {
    use gateway_core::routing::ProviderModelContent;
    use provider_openai::credential::ResponsesTransport;
    use provider_openai::transport::CodexCatalogCapabilityEvidence;
    use serde_json::json;

    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
        .await;
    let original = json!({"slug":"z-unknown-reasoning", "display_name":"Unknown reasoning"});
    let second = json!({
        "slug":"vendor/model", "display_name":"Second upstream model",
        "default_reasoning_level":"high",
        "supported_reasoning_levels":[{"effort":"high","description":"Upstream description"}]
    });
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with({
            let native = json!({"models":[original, second]});
            move |request: &Request| {
                if request.url.query() == Some("client_version=0.144.0") {
                    ResponseTemplate::new(200).set_body_json(&native)
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({"data":[
                        {"id":"z-unknown-reasoning"}, {"id":"vendor/model"}
                    ]}))
                }
            }
        })
        .expect(1)
        .mount(&upstream)
        .await;
    let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
    let snapshot = service
        .refresh_catalogs()
        .await
        .expect("native background directory");
    assert_eq!(
        snapshot.models()[0].capabilities().reasoning(),
        CodexCatalogCapabilityEvidence::Unknown
    );
    let account = store.account("acct_api").unwrap();
    assert_eq!(
        service.cached_account_models(&account).unwrap().unwrap(),
        ["z-unknown-reasoning", "vendor/model"]
    );
    let models = service
        .client_model_catalog(&client_scope(&[account]))
        .await
        .unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        ["z-unknown-reasoning", "vendor/model"]
    );
    for (model, expected) in models.iter().zip([original, second]) {
        let ProviderModelContent::Native(payload) = &model.content else {
            panic!("native model expected")
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(payload.body()).unwrap(),
            expected
        );
    }
    upstream.verify().await;
}

#[tokio::test]
async fn api_key_catalog_retries_only_explicit_version_parameter_rejections() {
    use provider_openai::credential::ResponsesTransport;
    use serde_json::json;

    for (status, code, param, retry) in [
        (400, "unknown_parameter", "client_version", true),
        (422, "unsupported_parameter", "client_version", true),
        (400, "unknown_parameter", "other", false),
        (400, "invalid_value", "client_version", false),
        (429, "unsupported_parameter", "client_version", false),
    ] {
        let upstream = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        store
            .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
            .await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(query_param("client_version", "0.144.0"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(json!({"error":{"code":code,"param":param}})),
            )
            .expect(1)
            .mount(&upstream)
            .await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(|request: &Request| request.url.query().is_none())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"api-model"}]})),
            )
            .expect(u64::from(retry))
            .mount(&upstream)
            .await;
        let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
        let account = store.account("acct_api").unwrap();
        let result = service.refresh_account_catalog(account.id()).await;
        if retry {
            assert_eq!(result.unwrap().models(), ["api-model"]);
        } else {
            assert!(matches!(
                result,
                Err(CodexCredentialCatalogError::Upstream { .. })
            ));
        }
        upstream.verify().await;
    }
}

#[tokio::test]
async fn api_key_client_catalog_distinguishes_filtered_empty_from_fetch_failure() {
    use gateway_core::account::AccountModelAccess;
    use provider_openai::credential::ResponsesTransport;
    for status in [200, 503] {
        let upstream = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        store
            .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
            .await;
        store.set_model_access(
            "acct_api",
            AccountModelAccess::new(
                gateway_core::account::AccountModelAccessMode::Allowlist,
                vec!["logical-model".into()],
            )
            .unwrap(),
        );
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(serde_json::json!({"data":[{"id":"wire-model"}]})),
            )
            .expect(1)
            .mount(&upstream)
            .await;
        let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
        assert_eq!(service.refresh_catalogs().await.is_ok(), status == 200);
        let result = service
            .client_model_catalog(&client_scope(&[store.account("acct_api").unwrap()]))
            .await;
        if status == 200 {
            assert!(
                result
                    .expect("success filtered by account policy")
                    .is_empty()
            );
        } else {
            assert!(
                result.is_err(),
                "fetch failure must not look like a successful empty catalog"
            );
        }
        upstream.verify().await;
    }
}

#[tokio::test]
async fn api_key_client_catalog_keeps_native_sources_stable_and_account_scoped() {
    use gateway_core::routing::ProviderModelContent;
    use provider_openai::credential::ResponsesTransport;
    use serde_json::json;

    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    let mut accounts = Vec::new();
    for (id, delay, body) in [
        ("acct_a", 0, json!({"data":[{"id":"shared"}]})),
        (
            "acct_b",
            30,
            json!({"models":[{"slug":"shared", "display_name":"First native", "default_reasoning_level":"high"}]}),
        ),
        (
            "acct_c",
            0,
            json!({"models":[{"slug":"shared", "display_name":"Second native", "default_reasoning_level":"low"}]}),
        ),
    ] {
        store
            .seed_api_key(
                id,
                format!("{}/{id}", upstream.uri()),
                ResponsesTransport::Http,
            )
            .await;
        accounts.push(store.account(id).unwrap());
        Mock::given(method("GET"))
            .and(path(format!("/{id}/models")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(body)
                    .set_delay(std::time::Duration::from_millis(delay)),
            )
            .expect(1)
            .mount(&upstream)
            .await;
    }
    let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
    service.refresh_catalogs().await.unwrap();
    for (scope_accounts, expected) in [
        (&accounts[..], "First native"),
        (&accounts[2..], "Second native"),
    ] {
        let models = service
            .client_model_catalog(&client_scope(scope_accounts))
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        let ProviderModelContent::Native(payload) = &models[0].content else {
            panic!("native catalog must take precedence over plain IDs")
        };
        let document: serde_json::Value = serde_json::from_slice(payload.body()).unwrap();
        assert_eq!(document["display_name"], expected);
    }
    upstream.verify().await;
}

#[tokio::test]
async fn api_key_client_catalog_cache_is_invalidated_by_credential_revision() {
    use gateway_core::account::{CredentialCasOutcome, CredentialCasUpdate, ProviderAccountUpdate};
    use provider_openai::credential::ResponsesTransport;

    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
        .await;
    let account = store.account("acct_api").unwrap();
    let scope = client_scope(std::slice::from_ref(&account));
    let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
    for model in ["before-rotation", "after-rotation"] {
        upstream.reset().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"models":[{
                    "slug":model, "display_name":model
                }]})),
            )
            .expect(1)
            .mount(&upstream)
            .await;
        assert!(service.client_model_catalog(&scope).await.is_err());
        service.refresh_catalogs().await.unwrap();
        let models = service.client_model_catalog(&scope).await.unwrap();
        assert_eq!(models[0].model.as_str(), model);
        upstream.verify().await;
        if model == "before-rotation" {
            let loaded = store.load_current_credential(account.id()).await.unwrap();
            let update = CredentialCasUpdate::new(
                account.id().clone(),
                account.revision(),
                ProviderAccountUpdate {
                    account_id: account.id().clone(),
                    name: account.name().to_owned(),
                    email: None,
                    plan_type: None,
                },
                loaded.credential,
                false,
                None,
                None,
            )
            .unwrap();
            assert!(matches!(
                store.compare_and_swap_credential(update).await.unwrap(),
                CredentialCasOutcome::Updated(_)
            ));
        }
    }
}

#[tokio::test]
async fn api_key_catalog_hides_only_known_specialized_models_in_codex_picker() {
    use provider_openai::credential::ResponsesTransport;
    use provider_openai::transport::CodexCatalogCapabilityEvidence;

    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
        .await;
    let picker_visibility = [
        ("gpt-5.4", true),
        ("ft:gpt-4.1-mini:example:custom:abc", true),
        ("vendor/model", true),
        ("gpt-image-2-custom", true),
        ("gpt-image-2", false),
        ("gpt-image-2-2026-04-21", false),
        ("gpt-live-1", false),
        ("gpt-realtime-2", false),
        ("tts-1", false),
    ];
    let ids = picker_visibility.map(|(id, _)| id);
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": ids.map(|id| serde_json::json!({"id": id}))
        })))
        .mount(&upstream)
        .await;
    let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
    let snapshot = service.refresh_catalogs().await.expect("standard API IDs");
    assert_eq!(
        snapshot
            .models()
            .iter()
            .map(|model| model.request_model().as_str())
            .collect::<Vec<_>>(),
        ids
    );
    for model in snapshot.models() {
        assert_eq!(
            model.capabilities().reasoning(),
            CodexCatalogCapabilityEvidence::Unknown
        );
        assert_eq!(model.limits().context_window_tokens(), None);
    }
    let account = store.account("acct_api").expect("API account");
    let models = service
        .client_model_catalog(&client_scope(&[account]))
        .await
        .expect("client catalog");
    let actual_visibility = models.iter().map(|model| {
        let gateway_core::routing::ProviderModelContent::Adapted(presentation) = &model.content
        else {
            panic!("plain API IDs must remain adapted")
        };
        assert!(presentation.supported_reasoning_efforts().is_empty());
        assert_eq!(presentation.default_reasoning_effort(), None);
        (model.model.as_str(), !presentation.hidden())
    });
    assert_eq!(
        actual_visibility.collect::<std::collections::BTreeMap<_, _>>(),
        picker_visibility.into_iter().collect()
    );
}

#[tokio::test]
async fn api_key_catalog_rejects_invalid_ids_and_duplicates() {
    use provider_openai::credential::ResponsesTransport;

    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
        .await;
    let service = service_with_catalog_cache(&store, upstream.uri(), catalog_cache());
    for invalid in [
        String::new(),
        "__reserved".to_owned(),
        "model\nname".to_owned(),
        " model".to_owned(),
        "x".repeat(257),
        "valid-model".to_owned(),
    ] {
        upstream.reset().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id":"valid-model"}, {"id":invalid}]
            })))
            .mount(&upstream)
            .await;
        assert!(
            service.refresh_catalogs().await.is_err(),
            "invalid entry accepted: {invalid:?}"
        );
        assert!(service.cached().expect("cache").is_none());
    }
}

#[tokio::test]
async fn api_key_catalog_does_not_replace_native_metadata_for_shared_models() {
    use gateway_core::routing::ProviderModelContent;
    use provider_openai::credential::ResponsesTransport;

    let oauth = MockServer::start().await;
    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    let oauth_account = seed_account(&store, "acct_oauth").await;
    Mock::given(method("GET")).and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"models":[{
            "slug":"gpt-5.4", "display_name":"Official Model", "context_window":272000,
            "input_modalities":["text","image"], "supported_reasoning_levels":[{"effort":"high"}]
        }]})))
        .mount(&oauth).await;
    let service = service_with_catalog_cache(&store, oauth.uri(), catalog_cache());
    let before = service.refresh_catalogs().await.expect("OAuth directory");
    store
        .seed_api_key("acct_api", upstream.uri(), ResponsesTransport::Http)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data":[{"id":"gpt-5.4"}]})),
        )
        .mount(&upstream)
        .await;

    let after = service.refresh_catalogs().await.expect("mixed directory");
    assert_eq!(
        after.models(),
        before.models(),
        "preserve the complete native model"
    );

    let api_account = store.account("acct_api").expect("API account");
    assert_eq!(
        service.cached_account_models(&api_account).unwrap(),
        service.cached_account_models(&oauth_account).unwrap()
    );
    let mixed = service
        .client_model_catalog(&client_scope(&[oauth_account, api_account.clone()]))
        .await
        .expect("mixed client catalog");
    assert_eq!(mixed.len(), 1);
    let ProviderModelContent::Native(payload) = &mixed[0].content else {
        panic!("native metadata expected")
    };
    assert_eq!(payload, before.models()[0].document());
    let api_only = service
        .client_model_catalog(&client_scope(std::slice::from_ref(&api_account)))
        .await
        .expect("API client catalog");
    assert!(matches!(
        api_only[0].content,
        ProviderModelContent::Adapted(_)
    ));
    let (api_documents, _) = service
        .account_catalog_documents(&api_account)
        .await
        .expect("target API account directory");
    assert_eq!(api_documents.len(), 1);
    assert_ne!(api_documents[0].document().protocol(), "codex");
}

#[tokio::test]
async fn account_catalog_documents_keep_native_objects_of_the_account_plan() {
    let store = Arc::new(MemoryAccountStore::default());
    let plus = seed_account_with_plan(&store, "acct_catalog_plus", "plus").await;
    let pro = seed_account_with_plan(&store, "acct_catalog_pro", "pro").await;
    let server = MockServer::start().await;
    for (account, unique_model, name, context_window) in [
        ("acct_catalog_plus", "gpt-plus", "Plus", 128_000),
        ("acct_catalog_pro", "gpt-pro", "Pro", 272_000),
    ] {
        Mock::given(method("GET"))
            .and(path("/codex/models"))
            .and(header("authorization", format!("Bearer access-{account}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [
                    {"slug": unique_model, "display_name": name, "context_window": context_window},
                    {"slug": "gpt-shared", "display_name": name, "context_window": context_window}
                ]
            })))
            .expect(2)
            .mount(&server)
            .await;
    }
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    let snapshot = service
        .refresh_catalogs()
        .await
        .expect("shared union snapshot");
    assert_eq!(snapshot.models().len(), 3);
    assert_eq!(snapshot.models()[1].display_name(), "Plus");

    let (models, _) = service
        .account_catalog_documents(&pro)
        .await
        .expect("pro catalog");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].request_model().as_str(), "gpt-pro");
    assert_eq!(models[1].request_model().as_str(), "gpt-shared");
    // 导出的目录文件靠原生对象携带上下文窗口等元数据，正文必须原样保留
    assert_eq!(models[0].document().protocol(), "codex");
    let document: serde_json::Value =
        serde_json::from_slice(models[1].document().body()).expect("native document");
    assert_eq!(document["context_window"], 272_000);
    assert_eq!(document["display_name"], "Pro");

    // 另一套餐的账号不能拿到别的套餐条目，否则客户端会列出自己用不了的模型
    let (plus_models, _) = service
        .account_catalog_documents(&plus)
        .await
        .expect("plus catalog");
    assert_eq!(plus_models.len(), 2);
    assert_eq!(plus_models[0].request_model().as_str(), "gpt-plus");
    let plus_document: serde_json::Value =
        serde_json::from_slice(plus_models[1].document().body()).expect("plus native document");
    assert_eq!(plus_document["context_window"], 128_000);

    server.verify().await;
}

#[tokio::test]
async fn disabled_account_can_export_native_catalog_without_a_cached_snapshot() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = seed_account(&store, "acct_disabled_export").await;
    store
        .set_enabled(account.id(), false)
        .await
        .expect("disable account");
    let disabled = store
        .account("acct_disabled_export")
        .expect("disabled account");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .and(header(
            "authorization",
            "Bearer access-acct_disabled_export",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": [{"slug": "gpt-5.4", "display_name": "GPT-5.4", "context_window": 272000}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());

    let (models, _) = service
        .account_catalog_documents(&disabled)
        .await
        .expect("disabled account export");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].document().protocol(), "codex");
    server.verify().await;
}

#[tokio::test]
async fn catalog_account_membership_changes_advance_generation_without_model_changes() {
    use std::collections::BTreeSet;
    let store = Arc::new(MemoryAccountStore::default());
    let first = seed_account(&store, "acct_source_a").await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": [{"slug": "gpt-5.4", "display_name": "GPT-5.4"}]
        })))
        .expect(2)
        .mount(&server)
        .await;
    let service = service_with_catalog_cache(&store, server.uri(), catalog_cache());
    let initial = service.refresh_catalogs().await.unwrap();
    let generation = service.catalog_generation();
    let second = seed_account(&store, "acct_source_b").await;
    let refreshed = service.refresh_catalogs().await.unwrap();
    assert_eq!(initial.models(), refreshed.models());
    assert!(service.catalog_generation() > generation);
    assert_eq!(
        initial.model_catalog_accounts()["gpt-5.4"],
        BTreeSet::from([first.id().clone()])
    );
    assert_eq!(
        refreshed.model_catalog_accounts()["gpt-5.4"],
        BTreeSet::from([first.id().clone(), second.id().clone()])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_thousand_restored_accounts_serve_concurrent_native_reads_while_upstream_stalls() {
    use futures::{StreamExt as _, stream};
    use gateway_core::account::AccountCatalogObservation;
    use std::time::{Duration, Instant, SystemTime};
    let store = Arc::new(MemoryAccountStore::default());
    let document = [&[0_u8][..], OFFICIAL_FIXTURE].concat();
    let mut accounts = Vec::new();
    for index in 0..10_000 {
        let account = seed_account(&store, &format!("acct_load_{index:05}")).await;
        assert!(
            store
                .save_account_catalog(
                    &AccountCatalogObservation {
                        account_id: account.id().clone(),
                        revision: account.revision(),
                        plan: account.plan_type().map(str::to_owned),
                        observed_at: SystemTime::now(),
                        etag: None
                    },
                    &document
                )
                .await
                .unwrap()
        );
        accounts.push(account);
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(503).set_delay(Duration::from_secs(30)))
        .expect(1)
        .mount(&server)
        .await;
    let catalog = Arc::new(service_with_catalog_cache(
        &store,
        server.uri(),
        catalog_cache(),
    ));
    let start = Instant::now();
    catalog.restore().await.unwrap();
    let restore = start.elapsed();
    catalog.refresh_catalogs().await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
    let scope = Arc::new(client_scope(&accounts));
    catalog
        .observe_response_etag(&accounts[0], "\"load-change\"")
        .unwrap();
    let refresh = tokio::spawn({
        let catalog = catalog.clone();
        async move { catalog.refresh_catalogs().await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    const REQUESTS: usize = 2048;
    let start = Instant::now();
    let reads = stream::iter(0..REQUESTS)
        .map(|_| {
            let catalog = catalog.clone();
            let scope = scope.clone();
            tokio::spawn(async move {
                let started = Instant::now();
                let models = catalog.client_model_catalog(&scope).await.unwrap();
                assert!(!models.is_empty());
                started.elapsed()
            })
        })
        .buffer_unordered(64)
        .collect::<Vec<_>>();
    let mut samples = tokio::time::timeout(Duration::from_secs(10), reads)
        .await
        .unwrap()
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    samples.sort_unstable();
    eprintln!(
        "catalog load: 10000 accounts, {REQUESTS} successful requests, concurrency 64, restore={} ms, elapsed={} ms, p50={} us, p95={} us, p99={} us",
        restore.as_millis(),
        start.elapsed().as_millis(),
        samples[REQUESTS / 2].as_micros(),
        samples[REQUESTS * 95 / 100].as_micros(),
        samples[REQUESTS * 99 / 100].as_micros()
    );
    assert!(!refresh.is_finished());
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    server.verify().await;
}
