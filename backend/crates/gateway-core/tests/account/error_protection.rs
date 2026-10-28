//! 账号保护的任意错误规则、缺失语义与匹配资源上限

use gateway_core::account::*;

fn sample_policy() -> AccountErrorProtection {
    serde_json::from_value(serde_json::json!({
        "enabled":true,"threshold":2,"windowSeconds":60,"durationSeconds":300,
        "probeEnabled":true,"probeModel":null,
        "rules":[{"id":"custom-code","name":"自定义拒绝","conditions":[
            {"field":"provider","operator":"equals","values":["vendor"],"ignoreCase":false},
            {"field":"status","operator":"equals","values":["400","409"],"ignoreCase":false},
            {"field":"code","operator":"equals","values":["plain_custom_error"],"ignoreCase":true}
        ]}]
    }))
    .unwrap()
}

fn facts() -> AccountErrorFacts<'static> {
    AccountErrorFacts {
        provider: "vendor",
        status: Some(400),
        code: Some(" PLAIN_CUSTOM_ERROR "),
        error_type: None,
        message: Some("anything"),
    }
}

#[test]
fn rules_share_or_candidates_and_require_every_condition() {
    let mut policy = sample_policy();
    policy.rules.push(AccountErrorRule {
        id: "message".into(),
        name: "消息片段".into(),
        conditions: vec![AccountErrorCondition {
            field: AccountErrorField::Message,
            operator: AccountErrorOperator::Contains,
            values: vec![Some("THING".into())],
            ignore_case: true,
        }],
    });
    assert!(policy.validate().is_ok());
    let compiled = policy.compile();
    assert_eq!(compiled.matching_rules(facts()).len(), 2);
    assert_eq!(
        compiled
            .matching_rules(AccountErrorFacts {
                status: Some(401),
                ..facts()
            })
            .len(),
        1
    );
    assert_eq!(
        compiled
            .matching_rules(AccountErrorFacts {
                provider: "elsewhere",
                message: None,
                ..facts()
            })
            .len(),
        0
    );
}

#[test]
fn absent_empty_and_case_are_explicit() {
    let mut policy = sample_policy();
    policy.rules[0].conditions = vec![AccountErrorCondition {
        field: AccountErrorField::Type,
        operator: AccountErrorOperator::Equals,
        values: vec![None],
        ignore_case: false,
    }];
    assert_eq!(policy.matching_rules(facts()).len(), 1);
    assert!(
        policy
            .matching_rules(AccountErrorFacts {
                error_type: Some(""),
                ..facts()
            })
            .is_empty()
    );
    policy.rules[0].conditions[0].values = vec![Some("".into())];
    assert!(policy.matching_rules(facts()).is_empty());
    assert_eq!(
        policy
            .matching_rules(AccountErrorFacts {
                error_type: Some("  "),
                ..facts()
            })
            .len(),
        1
    );
}

#[test]
fn message_contains_is_bounded_but_equals_uses_complete_trimmed_value() {
    let mut policy = sample_policy();
    policy.rules[0].conditions = vec![AccountErrorCondition {
        field: AccountErrorField::Message,
        operator: AccountErrorOperator::Contains,
        values: vec![Some("tail".into())],
        ignore_case: false,
    }];
    let long = format!("{}tail", "x".repeat(8192));
    assert!(
        policy
            .matching_rules(AccountErrorFacts {
                message: Some(&long),
                ..facts()
            })
            .is_empty()
    );
    let unicode = format!("{}tail", "界".repeat(2730));
    assert!(
        policy
            .matching_rules(AccountErrorFacts {
                message: Some(&unicode),
                ..facts()
            })
            .is_empty()
    );
    policy.rules[0].conditions[0].operator = AccountErrorOperator::Equals;
    let padded = format!("{}tail", " ".repeat(9000));
    assert_eq!(
        policy
            .matching_rules(AccountErrorFacts {
                message: Some(&padded),
                ..facts()
            })
            .len(),
        1
    );
    let full = format!("tail{}", "x".repeat(8192));
    assert!(
        policy
            .matching_rules(AccountErrorFacts {
                message: Some(&full),
                ..facts()
            })
            .is_empty()
    );
}

#[test]
fn validation_rejects_broad_rules_and_excessive_text() {
    let mut policy = sample_policy();
    policy.rules[0].conditions.truncate(1);
    assert!(policy.validate().is_err());
    let mut policy = sample_policy();
    policy.rules[0].conditions = vec![AccountErrorCondition {
        field: AccountErrorField::Message,
        operator: AccountErrorOperator::Contains,
        values: vec![None],
        ignore_case: false,
    }];
    assert!(policy.validate().is_err());
    policy.rules[0].conditions[0].values = vec![Some(" ".into())];
    assert!(policy.validate().is_err());
    policy.rules[0].conditions[0].values = vec![Some("x".repeat(1000)); 16];
    policy.rules[0].conditions = vec![policy.rules[0].conditions[0].clone(); 4];
    assert!(policy.validate().is_ok());
    let condition = policy.rules[0].conditions[0].clone();
    policy.rules[0].conditions.push(condition);
    assert!(policy.validate().is_err());
}

#[test]
fn policy_fingerprint_tracks_every_semantic_setting() {
    let policy = sample_policy();
    assert_eq!(policy.fingerprint(), policy.compile().fingerprint());
    let mut changed = policy.clone();
    changed.enabled = false;
    assert_ne!(policy.fingerprint(), changed.fingerprint());
    let mut changed = policy.clone();
    changed.rules[0].conditions[0].ignore_case = true;
    assert_ne!(policy.fingerprint(), changed.fingerprint());
    let mut changed = policy.clone();
    changed.probe_model = Some("probe-model".into());
    assert_ne!(policy.fingerprint(), changed.fingerprint());
}
