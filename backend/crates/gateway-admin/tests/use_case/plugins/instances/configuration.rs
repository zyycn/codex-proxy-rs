//! 实例敏感配置的逐字段更新、版本基准与提交隔离测试

use super::*;
use secrecy::ExposeSecret;

fn secret_values(instance: &PluginInstance) -> serde_json::Value {
    serde_json::Value::Object(
        instance
            .secrets
            .iter()
            .map(|(name, value)| (name.clone(), json!(value.expose_secret())))
            .collect(),
    )
}

#[tokio::test]
async fn secret_changes_apply_to_current_or_target_version_configuration() {
    for (target, saved_version) in [
        (OLD_ARTIFACT, false),
        (NEW_ARTIFACT, false),
        (NEW_ARTIFACT, true),
    ] {
        let token = if saved_version {
            "saved-token"
        } else {
            "current-token"
        };
        let untouched = if saved_version {
            "saved-value"
        } else {
            "current-value"
        };
        for (changes, expected) in [
            (json!({}), json!({"token":token,"untouched":untouched})),
            (
                json!({"adminKey":"new-key"}),
                json!({"token":token,"untouched":untouched,"adminKey":"new-key"}),
            ),
            (
                json!({"token":"new-token"}),
                json!({"token":"new-token","untouched":untouched}),
            ),
            (json!({"token":null}), json!({"untouched":untouched})),
            (
                json!({"token":null,"adminKey":"new-key","absent":null}),
                json!({"untouched":untouched,"adminKey":"new-key"}),
            ),
        ] {
            let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
            {
                let mut data = fixture.data.lock().unwrap();
                data.snapshot.instances[0].secrets = BTreeMap::from([
                    ("token".into(), "current-token".into()),
                    ("untouched".into(), "current-value".into()),
                ]);
                if saved_version {
                    data.history.get_mut(NEW_ARTIFACT).unwrap().secrets = BTreeMap::from([
                        ("token".into(), "saved-token".into()),
                        ("untouched".into(), "saved-value".into()),
                    ]);
                } else {
                    data.history.clear();
                }
            }
            let before = fixture.snapshot();
            let mut request = input();
            request.artifact_sha256 = target.into();
            request.expected_revision = Some(10);
            request.secrets = serde_json::from_value(changes).unwrap();
            let result = service(fixture.clone(), Arc::new(Published::default()))
                .configure_instance(Some(&before.instances[0].id), request, &context())
                .await
                .unwrap();
            assert_eq!(secret_values(&result.instance), expected);
            assert_eq!(secret_values(&fixture.snapshot().instances[0]), expected);
        }
    }
}

#[tokio::test]
async fn failed_secret_changes_leave_saved_values_unchanged() {
    for failure in ["stale", "preparation", "size"] {
        let fixture = LifecycleFixture::new(MigrationBehavior::Succeed);
        fixture.data.lock().unwrap().snapshot.instances[0].secrets =
            BTreeMap::from([("token".into(), "saved-token".into())]);
        let before = fixture.snapshot();
        let mut request = input();
        request.artifact_sha256 = OLD_ARTIFACT.into();
        request.expected_revision = Some(if failure == "stale" { 9 } else { 10 });
        request.secrets = BTreeMap::from([
            ("token".into(), None),
            (
                "adminKey".into(),
                Some(
                    if failure == "size" {
                        "x".repeat(8193)
                    } else {
                        "new-key".into()
                    }
                    .into(),
                ),
            ),
        ]);
        fixture.data.lock().unwrap().fail_preparation = failure == "preparation";
        let result = service(fixture.clone(), Arc::new(Published::default()))
            .configure_instance(Some(&before.instances[0].id), request, &context())
            .await;
        assert!(result.is_err(), "{failure} must reject the update");
        assert_eq!(
            secret_values(&fixture.snapshot().instances[0]),
            secret_values(&before.instances[0])
        );
        assert!(fixture.data.lock().unwrap().saves.is_empty());
    }
}
