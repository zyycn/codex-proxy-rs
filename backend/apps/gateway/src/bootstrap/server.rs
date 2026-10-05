//! 服务模式完成全部组装后交给 Host 监听，并在请求和 Worker 排空后关闭插件

use gateway_host::ConfigError;

use super::{
    BootstrapError, GatewayConfig,
    plugins::{ManagementPorts, or_shutdown, prepare_plugins, service_middleware},
    startup::{Environment, Mode, Providers},
};

/// 按冻结顺序初始化全部 Bundle，并把进程阻塞权交给 Host
pub async fn run() -> Result<(), BootstrapError> {
    let config = gateway_host::load_config::<GatewayConfig>()?;
    let environment = Environment::initialize(config.host, config.store, Mode::Server).await?;
    let mut providers = Providers::initialize(config.openai, &environment).await?;
    let Environment {
        host,
        mut store,
        timezone,
        plugin_cache,
        version,
        plugin_http,
    } = environment;
    let plugin_runtime = prepare_plugins(&store, plugin_cache, &version, plugin_http.clone())?;
    let core = gateway_core::prepare(
        store.core_ports(),
        providers.core,
        Some(plugin_runtime.clone()),
        Some(plugin_runtime.observer_registry()),
        Some(plugin_runtime.policy_registry()),
        Some(plugin_runtime.middleware_registry()),
        Some(plugin_runtime.frontend_authentication_registry()),
    );
    let management = ManagementPorts::new(&store, providers.admin.clone(), core.snapshot_control());
    or_shutdown!(plugin_runtime, management.bind(&plugin_runtime));
    let mut core = or_shutdown!(plugin_runtime, core.activate().await);
    let nested_models = core.nested_model_execution_port();
    let affinity_lookup = core.affinity_lookup_port();
    or_shutdown!(
        plugin_runtime,
        plugin_runtime.bind_model_ports(&nested_models, &affinity_lookup)
    );
    host.report_startup_ready("Core");
    let host_version = or_shutdown!(
        plugin_runtime,
        version
            .trim_start_matches('v')
            .parse()
            .map_err(|_| ConfigError::InvalidField("host.version"))
    );
    let plugin_inspector = std::sync::Arc::new(gateway_plugin_runtime::PackageInspector::new(
        gateway_plugin_runtime::PackageLimits::default(),
        host_version,
    ));
    let plugin_distribution = std::sync::Arc::new(or_shutdown!(
        plugin_runtime,
        gateway_host::plugin_distribution::HttpPluginDistribution::new(plugin_http)
    ));
    let mut admin = or_shutdown!(
        plugin_runtime,
        gateway_admin::initialize_with_plugin_accounts(
            config.admin,
            config.client,
            store.admin_ports(),
            gateway_admin::AdminRuntimePorts {
                timezone,
                service_middleware: service_middleware(core.snapshots(), &plugin_runtime),
                providers: providers.admin,
                plugin_preparation: plugin_runtime.clone(),
                plugin_management: plugin_runtime.clone(),
                published_snapshot: core.snapshots(),
                plugin_inspector,
                plugin_distribution,
                pricing_source: std::sync::Arc::new(gateway_host::pricing::ModelsDevPricing),
                snapshot: core.snapshot_control(),
                account_probe: core.account_probe(),
                proxy_probe: host.proxy_probe(provider_openai::build_reqwest_client_with_custom_ca),
                client_distribution: host.client_distribution_resolver(),
                system: host.system_operations(),
                client_key_verifier: core.client_key_verifier(),
            },
            management.accounts.clone(),
        )
        .await
    );
    let official_files = host.official_plugin_release_files();
    let official_identity = host.official_plugin_release_identity();
    let official_import = or_shutdown!(
        plugin_runtime,
        admin
            .services()
            .plugins()
            .import_official_release(
                official_files.as_ref(),
                &official_identity,
                &gateway_admin::model::MutationContext {
                    actor: gateway_admin::model::MutationActor::System,
                    request_id: "startup-official-plugin-import".to_owned(),
                },
            )
            .await
    );
    if official_import.artifacts > 0 {
        host.report_startup_ready("Official plugins");
    }
    host.report_startup_ready("Admin");

    let mut probes = store.health_probes();
    probes.extend(core.health_probes());
    probes.push(host.logging_health_probe());
    let api = or_shutdown!(
        plugin_runtime,
        gateway_api::initialize(
            config.api,
            core.execution_service(),
            admin.services(),
            probes,
            host.worker_health(),
            host.connection_lifecycle(),
        )
    )
    .with_middleware({
        let snapshots = core.snapshots();
        let middleware = plugin_runtime.middleware_registry();
        move |snapshot| {
            if let Some(snapshot) = snapshot {
                return middleware.resolve(snapshot.extensions()?);
            }
            // 数据面未就绪时，管理与诊断路由仍可使用发布候选中的插件
            let diagnostic = snapshots.snapshot_for_diagnostics()?;
            middleware.resolve(diagnostic.extensions()?)
        }
    });
    let http_dispatcher = api.dispatcher();
    or_shutdown!(plugin_runtime, plugin_runtime.bind_http(&http_dispatcher));
    host.report_startup_ready("API");

    or_shutdown!(
        plugin_runtime,
        plugin_runtime.bind_services(&admin.services().public_services())
    );

    let mut plan = store.take_worker_contributions();
    plan.push(or_shutdown!(
        plugin_runtime,
        gateway_host::retention::worker(store.retention())
    ));
    plan.extend(core.take_worker_contributions());
    plan.extend(providers.openai.take_worker_contributions());
    plan.extend(providers.xai.take_worker_contributions());
    plan.extend(admin.take_worker_contributions());
    plan.push(or_shutdown!(
        plugin_runtime,
        plugin_runtime.maintenance_worker(core.snapshots())
    ));
    or_shutdown!(
        plugin_runtime,
        host.start_workers(plan, store.worker_leader_lease())
    );
    host.report_startup_ready("Workers");
    let served = host.serve(api.router()).await;
    plugin_runtime.shutdown().await;
    Ok(served?)
}
