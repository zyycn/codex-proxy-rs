//! 验证真实插件日志的调用关联、输入校验、脱敏与数量限制

use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use gateway_admin::{
    model::{
        Revision,
        plugins::instances::{PluginInstance, PluginInstanceSnapshot},
    },
    ports::plugins::{PluginPackageInspector, PluginPreparation},
};
use gateway_plugin_runtime::{
    PackageInspector, PackageLimits, PluginRuntime, PluginRuntimeConfig, RpcLimits,
};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<BTreeMap<String, String>>>>, Arc<Gate>);

#[derive(Default)]
struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn wait(&self) {
        // 测试回归为同步阻塞时也能自行退出，不能把测试运行器永久挂住
        let (mut released, _) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(30),
                |released| !*released,
            )
            .unwrap();
        *released = true;
        self.changed.notify_all();
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct Fields(BTreeMap<String, String>);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "gateway_plugin"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if event.metadata().target() == "gateway_plugin" {
            let mut fields = Fields(BTreeMap::from([(
                "level".into(),
                event.metadata().level().to_string(),
            )]));
            event.record(&mut fields);
            if fields
                .0
                .get("instance_id")
                .is_some_and(|id| id == "log-blocked")
            {
                self.1.wait();
            }
            self.0.lock().unwrap().push(fields.0);
        }
    }
}

#[tokio::test]
async fn real_worker_logs_are_correlated_sanitized_validated_and_bounded() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(capture.clone()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("callbacks.jsonl");
    let mut entries = ["debug", "info", "warn", "error"].map(|level| json!({
        "params":{"event":"fixture-secret-event-token","level":level,"fields":{
            "status":429,"authorization":"fixture-secret-token",
            "metadata":{"message":"fixture-private-body","credential":"fixture-nested-secret"},
            "fixture-private-key-name":"fixture-private-value"
        }}
    })).to_vec();
    for params in [
        json!({"event":"bad\nline"}),
        json!({"event":"x".repeat(65)}),
        json!({"event":"fixture.log","fields":{"message":"x".repeat(9000)}}),
        json!({"event":"fixture.log","fields": (0..33).map(|i| (format!("f{i}"),json!(1))).collect::<BTreeMap<_,_>>()}),
        json!({"event":"fixture.log","instance_id":"forged"}),
        json!({"event":"fixture.log","level":"trace"}),
    ] {
        entries.push(json!({"params":params}));
    }
    entries.push(json!({"params":{"event":"fixture.log"},"with_payload":true}));
    entries.push(json!({"params":{"event":"fixture.flood"},"repeat":128}));
    entries.push(json!({"params":{"event":"fixture.resumed"},"delay_ms":1100}));

    let artifact = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap())
        .inspect(crate::support::package(crate::support::worker()), None)
        .await
        .unwrap();
    let instance = PluginInstance {
        id: "log-fixture".into(),
        name: "日志验收".into(),
        artifact_sha256: artifact.metadata.sha256.clone(),
        enabled: true,
        trusted_process: true,
        configuration: json!({"log_method":"plugin.register","log_entries":entries,"log_marker":marker}),
        secrets: BTreeMap::new(),

        bindings: vec![],
        revision: Revision::new(1).unwrap(),
    };
    let snapshot = PluginInstanceSnapshot {
        config_revision: Revision::new(7).unwrap(),
        instances: vec![instance],
    };
    let store = Arc::new(crate::support::store::Store {
        artifacts: BTreeMap::from([(artifact.metadata.sha256.clone(), artifact)]),
        snapshot: Mutex::new(snapshot.clone()),
    });
    let runtime = PluginRuntime::new(
        store.clone(),
        store,
        PluginRuntimeConfig {
            cache_directory: directory.path().join("cache"),
            host_version: "1.0.0".parse().unwrap(),
            package_limits: PackageLimits::default(),
            rpc_limits: RpcLimits::default(),
            restart_circuit: Default::default(),
        },
        Arc::new(gateway_host::outbound::HttpClient::new().unwrap()),
        Arc::new(gateway_host::process::ProcessSupervisor::new(
            std::num::NonZeroUsize::new(4).unwrap(),
        )),
    );
    let generation = PluginPreparation::prepare(&runtime, snapshot.clone())
        .await
        .unwrap();
    let results: Vec<Value> =
        serde_json::from_str(std::fs::read_to_string(&marker).unwrap().trim()).unwrap();
    assert!(
        results[..4].iter().all(|result| result["recorded"] == true),
        "{:?}",
        &results[..4]
    );
    assert!(
        results[4..11]
            .iter()
            .all(|result| result["error"] == "invalid_input")
    );
    let suppressed = results
        .iter()
        .filter(|result| result["recorded"] == false)
        .count();
    assert!(suppressed > 0, "日志过量应丢弃，不把背压传播给业务调用");
    assert_eq!(results.last().unwrap()["recorded"], true);
    let accepted = results
        .iter()
        .filter(|result| result["recorded"] == true)
        .count();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let count = capture
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|fields| {
                    fields
                        .get("instance_id")
                        .is_some_and(|id| id == "log-fixture")
                })
                .count();
            if count == accepted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    {
        let captured = capture.0.lock().unwrap();
        let records = captured
            .iter()
            .filter(|fields| {
                fields
                    .get("instance_id")
                    .is_some_and(|id| id == "log-fixture")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            results
                .iter()
                .filter(|result| result["recorded"] == true)
                .count()
        );
        for record in &records {
            assert_eq!(record["plugin_id"], "test.example");
            assert_eq!(record["plugin_version"], "1.0.0");
            assert_eq!(record["generation"], "7");
            assert_ne!(record["call_id"], "0");
            assert!(!record["incarnation"].is_empty());
        }
        for level in ["DEBUG", "INFO", "WARN", "ERROR"] {
            assert!(records.iter().any(|record| record["level"] == level));
        }
        let first_fields: Value = serde_json::from_str(
            &records
                .iter()
                .find(|record| record["level"] == "ERROR")
                .unwrap()["fields"],
        )
        .unwrap();
        assert_eq!(first_fields["status"], 429);
        let encoded = format!("{records:?}");
        for secret in [
            "fixture-secret-event-token",
            "fixture-secret-token",
            "fixture-private-body",
            "fixture-nested-secret",
            "fixture-private-key-name",
            "fixture-private-value",
        ] {
            assert!(
                !encoded.contains(secret),
                "普通日志不得保留 fixture 敏感内容"
            );
        }
        assert!(
            records
                .iter()
                .any(|record| record["suppressed_logs"].parse::<u64>().unwrap() > 0)
        );
    }
    let release = ReleaseOnDrop(capture.1.clone());
    let blocked_marker = directory.path().join("blocked.jsonl");
    let mut blocked = snapshot.clone();
    blocked.config_revision = Revision::new(9).unwrap();
    blocked.instances[0].id = "log-blocked".into();
    blocked.instances[0].configuration = json!({
        "log_method":"plugin.register", "log_entries":[{"params":{"event":"fixture.blocked"},"repeat":32}],
        "log_marker":blocked_marker,
    });
    // 日志后端停住时，注册的业务结果和父调用结束仍须及时完成
    let blocked_generation = tokio::time::timeout(
        Duration::from_secs(10),
        PluginPreparation::prepare(&runtime, blocked),
    )
    .await
    .unwrap()
    .unwrap();
    let blocked_results: Vec<Value> =
        serde_json::from_str(std::fs::read_to_string(blocked_marker).unwrap().trim()).unwrap();
    assert_eq!(
        blocked_results
            .iter()
            .filter(|result| result["recorded"] == true)
            .count(),
        RpcLimits::default().maximum_callbacks
    );
    assert!(
        blocked_results
            .iter()
            .any(|result| result["recorded"] == false)
    );
    drop(blocked_generation);
    let next_marker = directory.path().join("next-generation.jsonl");
    let mut next = snapshot;
    next.config_revision = Revision::new(10).unwrap();
    next.instances[0].id = "log-next-generation".into();
    next.instances[0].configuration = json!({
        "log_method":"plugin.register", "log_entries":[{"params":{"event":"fixture.next"}}],
        "log_marker":next_marker,
    });
    let next_generation = tokio::time::timeout(
        Duration::from_secs(10),
        PluginPreparation::prepare(&runtime, next),
    )
    .await
    .unwrap()
    .unwrap();
    let next_results: Vec<Value> =
        serde_json::from_str(std::fs::read_to_string(next_marker).unwrap().trim()).unwrap();
    assert_eq!(
        next_results,
        [json!({"recorded":false})],
        "父调用结束与代次撤下不能提前释放仍阻塞的写入容量"
    );
    drop(next_generation);
    drop(release);
    drop(generation);
    tokio::time::timeout(Duration::from_secs(3), async {
        while std::fs::read_dir(directory.path().join("cache"))
            .unwrap()
            .next()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
