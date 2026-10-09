//! 隐私策略在快照与请求计划中的冻结、复用及关闭行为

use super::*;
use gateway_core::settings::privacy::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct Compiler(AtomicUsize);

#[derive(Debug)]
struct Compiled(serde_json::Value);

impl PrivacyPolicyCompiler for Compiler {
    fn compile(
        &self,
        policy: &CodexPrivacyPolicy,
    ) -> Result<Arc<dyn CompiledPrivacyPolicy>, PrivacyError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Compiled(policy.rules[0].value.clone())))
    }
}

impl CompiledPrivacyPolicy for Compiled {
    fn apply(
        &self,
        body: &mut serde_json::Value,
        _: &mut http::HeaderMap,
        _: &mut Option<String>,
        _: &dyn Fn() -> bool,
    ) -> Result<Vec<PrivacyRuleOutcome>, PrivacyError> {
        *body = self.0.clone();
        Ok(Vec::new())
    }
}

fn policy(value: &str) -> CodexPrivacyPolicy {
    CodexPrivacyPolicy {
        enabled: true,
        on_error: PrivacyFailureMode::RejectRequest,
        rules: vec![PrivacyRule {
            id: "rule".into(),
            name: "规则".into(),
            enabled: true,
            scope: PrivacyScope::RequestBody,
            selector: "$.metadata".into(),
            action: PrivacyAction::SetValue,
            pattern: None,
            replacement: String::new(),
            value: serde_json::json!(value),
            replace_all: false,
            case_insensitive: false,
            multi_line: false,
        }],
    }
}

fn compiled(snapshot: &RuntimeSnapshot) -> Option<Arc<dyn CompiledPrivacyPolicy>> {
    snapshot
        .plan_provider_endpoint(
            &ProviderKind::new("openai").unwrap(),
            None,
            &image_operation(),
            snapshot.all_account_scope(),
            &RoutingContext::default(),
        )
        .unwrap()
        .privacy()
}

#[test]
fn privacy_is_frozen_and_unrelated_request_settings_reuse_compiled_rules() {
    let compiler = Arc::new(Compiler::default());
    let snapshot = RuntimeSnapshot::new_with_privacy_compiler(
        ConfigRevision::new(1).unwrap(),
        settings().with_codex_privacy_policy(policy("first")),
        vec![ProviderKind::new("openai").unwrap()],
        vec![],
        vec![],
        Some(compiler.clone()),
    )
    .unwrap()
    .with_account_directory(account_directory());
    let original = compiled(&snapshot).unwrap();
    let limits = snapshot
        .with_settings(&snapshot.settings().clone().with_max_account_rotations(5))
        .unwrap();
    assert!(Arc::ptr_eq(&original, &compiled(&limits).unwrap()));
    assert_eq!(compiler.0.load(Ordering::SeqCst), 1);
    let changed = snapshot
        .with_settings(
            &snapshot
                .settings()
                .clone()
                .with_codex_privacy_policy(policy("second")),
        )
        .unwrap();
    let mut body = serde_json::Value::Null;
    original
        .apply(&mut body, &mut http::HeaderMap::new(), &mut None, &|| false)
        .unwrap();
    assert_eq!(body, "first");
    compiled(&changed)
        .unwrap()
        .apply(&mut body, &mut http::HeaderMap::new(), &mut None, &|| false)
        .unwrap();
    assert_eq!(body, "second");
    assert_eq!(compiler.0.load(Ordering::SeqCst), 2);
    for disable_rule in [false, true] {
        let mut policy = policy("unused");
        if disable_rule {
            policy.rules[0].enabled = false;
        } else {
            policy.enabled = false;
        }
        let disabled = snapshot
            .with_settings(
                &snapshot
                    .settings()
                    .clone()
                    .with_codex_privacy_policy(policy),
            )
            .unwrap();
        assert!(compiled(&disabled).is_none());
    }
}
