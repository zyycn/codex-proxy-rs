use serde_json::{Value, json};

#[tokio::test]
async fn command_parser_preserves_all_types_defaults_and_signed_nanosecond_boundaries() {
    let parameters = json!([
        {"name":"enabled","description":"启用","value_type":"bool"},
        {"name":"label","description":"名称","value_type":"string","required":true},
        {"name":"count","description":"数量","value_type":"int"},
        {"name":"wide","description":"编号","value_type":"int64"},
        {"name":"ratio","description":"比例","value_type":"float64"},
        {"name":"wait","description":"等待","value_type":"duration"},
        {"name":"secret","description":"私密默认","value_type":"string","sensitive":true,"default":{"type":"string","value":"hidden-default"}}
    ]);
    let (_directory, _store, runtime) = super::setup(super::configuration(parameters)).await;
    let commands = runtime.prepare_command_line().await.unwrap();
    let help = commands.help(Some("commands"), Some("inspect")).unwrap();
    assert!(!help.contains("hidden-default"));
    assert!(help.contains("已隐藏"));
    for (duration, expected) in [
        ("1h2m3.000000001s", 3_723_000_000_001_i64),
        ("-9223372036854775808ns", i64::MIN),
        (".5s", 500_000_000),
        ("0", 0),
    ] {
        let args = [
            "--enabled",
            "--label=a=b",
            "--count=-2147483648",
            "--wide=9223372036854775807",
            "--ratio=.125",
            "--wait",
            duration,
        ]
        .map(str::to_owned);
        let output = commands
            .execute("commands", "inspect", &args)
            .await
            .unwrap();
        let invocation: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(
            invocation["arguments"]["enabled"],
            json!({"type":"bool","value":true})
        );
        assert_eq!(invocation["arguments"]["label"]["value"], "a=b");
        assert_eq!(invocation["arguments"]["count"]["value"], i32::MIN);
        assert_eq!(invocation["arguments"]["wide"]["value"], i64::MAX);
        assert_eq!(invocation["arguments"]["ratio"]["value"], 0.125);
        assert_eq!(invocation["arguments"]["wait"]["value"], expected);
        assert_eq!(invocation["arguments"]["secret"]["value"], "hidden-default");
    }
    for invalid in [
        vec!["--label=private-sentinel", "--count=2147483648"],
        vec!["--label=private-sentinel", "--ratio=NaN"],
        vec!["--label=private-sentinel", "--ratio=inf"],
        vec!["--label=private-sentinel", "--wait=9223372036854775808ns"],
        vec!["--label=private-sentinel", "--wait=0.1ns"],
        vec!["--label=private-sentinel", "--wait=1.2.3s"],
        vec!["--label=private-sentinel", "--label=duplicate"],
        vec!["--label=private-sentinel", "--unknown"],
        vec!["--count=1"],
    ] {
        let args = invalid.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let error = commands
            .execute("commands", "inspect", &args)
            .await
            .err()
            .unwrap();
        assert!(matches!(
            error,
            gateway_plugin_runtime::PluginCommandError::Invalid(_)
        ));
        assert!(!error.to_string().contains("private-sentinel"));
    }
}

#[tokio::test]
async fn command_registration_rejects_reserved_duplicate_and_mistyped_parameters() {
    let (_directory, store, runtime) = super::setup(super::configuration(json!([]))).await;
    for parameters in [
        json!([{"name":"help","description":"保留名","value_type":"bool"}]),
        json!([{"name":"test","description":"默认类型错误","value_type":"int","default":{"type":"string","value":"bad"}}]),
        json!([{"name":"same","description":"重复","value_type":"bool"},{"name":"same","description":"重复","value_type":"bool"}]),
        json!([{"name":"bad","description":"\u{1b}[2J","value_type":"bool"}]),
    ] {
        store.snapshot.lock().unwrap().instances[0].configuration =
            super::configuration(parameters);
        let commands = runtime
            .prepare_command_line()
            .await
            .expect("invalid plugin is isolated from the host");
        assert!(commands.help(Some("commands"), None).is_err());
        let snapshot = store.snapshot.lock().unwrap().clone();
        assert!(
            gateway_admin::ports::plugins::PluginPreparation::prepare(
                &runtime,
                snapshot.config_revision,
                snapshot,
            )
            .await
            .is_err(),
            "invalid registration still rejects an explicit candidate"
        );
    }
    for name in ["help", "serve", "plugin", "version"] {
        let mut config = super::configuration(json!([]));
        config["command_registration"]["commands"][0]["name"] = json!(name);
        store.snapshot.lock().unwrap().instances[0].configuration = config;
        let commands = runtime
            .prepare_command_line()
            .await
            .expect("invalid plugin is isolated from the host");
        assert!(commands.help(Some("commands"), None).is_err());
        let snapshot = store.snapshot.lock().unwrap().clone();
        assert!(
            gateway_admin::ports::plugins::PluginPreparation::prepare(
                &runtime,
                snapshot.config_revision,
                snapshot,
            )
            .await
            .is_err(),
            "invalid registration still rejects an explicit candidate"
        );
    }
}
